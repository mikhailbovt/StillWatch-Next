#![forbid(unsafe_code)]

mod alerts;
mod backup;
mod cameras;
mod db;
mod evidence;
mod health;
mod legacy;
mod retention;
mod semantic;
#[cfg(test)]
mod tests;
mod trust;

use std::{
    io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use sw_domain::{
    Assessment, Clearance, DomainError, EvidenceStamp, Incident, NewIncident, OperatorAction,
    SourceScope, UnixMillis,
};
use thiserror::Error;
use tokio::sync::{Semaphore, oneshot, watch};
use uuid::Uuid;

use db::Database;

pub use backup::restore_backup;
pub use cameras::{CameraAction, CameraCommand, CameraLimits, CameraRecord, CameraResult};
pub use evidence::{
    EvidenceFailure, EvidenceImage, EvidenceOutcome, EvidenceResult, EvidenceWrite,
};
pub use health::{HealthIssue, HealthObservation};
pub use legacy::{LegacyImport, LegacyManifest, import_legacy};
pub use semantic::{SemanticOutcome, SemanticWork};
pub use trust::{DeviceAccess, PairingGrant};

const MAX_PAGE: usize = 200;
const MAX_CHANGES: usize = 100_000;
const MAX_SNAPSHOT: usize = 2_000;
const WRITE_WINDOW: Duration = Duration::from_secs(10);

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("data directory is held by another Core")]
    InUse,
    #[error("database schema is unsupported")]
    Schema,
    #[error("database writer queue is full")]
    Busy,
    #[error("database request expired while queued")]
    Deadline,
    #[error("database writer is unavailable. Retry writes with the original idempotency key")]
    Unavailable,
    #[error("incident does not exist")]
    NotFound,
    #[error("incident already exists with different creation facts")]
    IncidentConflict,
    #[error("incident revision changed to {current}")]
    Revision { current: u64 },
    #[error("idempotency key was previously used with another request")]
    Idempotency,
    #[error("change cursor expired")]
    CursorExpired,
    #[error("too many open incidents for a bounded snapshot")]
    SnapshotLimit,
    #[error("camera configuration exceeds active capture capacity")]
    CameraCapacity,
    #[error("invalid storage request")]
    Invalid,
    #[error("the device credential or pairing challenge is invalid, expired or revoked")]
    Credential,
    #[error("the current device or pairing quota has been reached")]
    DeviceCapacity,
    #[error(transparent)]
    Domain(#[from] DomainError),
    #[error("storage I/O failure")]
    Io(#[from] io::Error),
    #[error("SQLite operation failed")]
    Sql(#[from] rusqlite::Error),
    #[error("invalid or damaged stored metadata")]
    Data(#[from] serde_json::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Identity {
    pub instance_id: Uuid,
    pub generation: Uuid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChangeCursor {
    pub generation: Uuid,
    pub sequence: u64,
}

#[derive(Debug)]
pub struct Change {
    pub cursor: ChangeCursor,
    pub opened: bool,
    pub incident: Incident,
}

#[derive(Debug)]
pub struct Snapshot {
    pub cursor: ChangeCursor,
    pub incidents: Vec<Incident>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HistoryCursor {
    pub created_at: UnixMillis,
    pub id: Uuid,
}

#[derive(Debug)]
pub struct IncidentPage {
    pub incidents: Vec<Incident>,
    pub next: Option<HistoryCursor>,
}

#[derive(Debug)]
pub struct OperatorCommand {
    pub incident_id: Uuid,
    pub actor_id: Uuid,
    pub idempotency_key: String,
    pub expected_revision: u64,
    pub action: OperatorAction,
}

#[derive(Debug)]
pub struct CommandResult {
    pub incident: Incident,
    pub replayed: bool,
}

#[derive(Clone, Debug)]
pub struct StoreConfig {
    pub evidence_quota_bytes: u64,
    pub queue_capacity: usize,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            evidence_quota_bytes: 500 * 1024 * 1024,
            queue_capacity: 64,
        }
    }
}

type Job = Box<dyn FnOnce(&mut Database) + Send>;

struct Worker {
    requests: Option<SyncSender<Job>>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.requests.take();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[derive(Clone)]
pub struct Store {
    worker: Arc<Worker>,
    identity: Identity,
    wake: watch::Receiver<()>,
    evidence_inflight: Arc<Semaphore>,
}

impl Store {
    pub fn open(root: &Path, config: StoreConfig) -> Result<Self, StoreError> {
        if !(1..=4096).contains(&config.queue_capacity)
            || config.evidence_quota_bytes > i64::MAX as u64
        {
            return Err(StoreError::Invalid);
        }
        let root = root.to_owned();
        let (requests, incoming) = mpsc::sync_channel::<Job>(config.queue_capacity);
        let (boot, startup) = mpsc::sync_channel(1);
        let (notify, wake) = watch::channel(());
        let handle = thread::Builder::new()
            .name("stillwatch-sqlite".into())
            .spawn(
                move || match Database::open(&root, config.evidence_quota_bytes, notify) {
                    Ok(mut db) => {
                        if boot.send(Ok(db.identity)).is_ok() {
                            for job in incoming {
                                job(&mut db);
                            }
                        }
                    }
                    Err(error) => {
                        let _ = boot.send(Err(error));
                    }
                },
            )?;
        let worker = Arc::new(Worker {
            requests: Some(requests),
            handle: Some(handle),
        });
        let identity = startup.recv().map_err(|_| StoreError::Unavailable)??;
        Ok(Self {
            worker,
            identity,
            wake,
            evidence_inflight: Arc::new(Semaphore::new(64 * 1024 * 1024)),
        })
    }

    pub fn identity(&self) -> Identity {
        self.identity
    }

    pub fn subscribe(&self) -> watch::Receiver<()> {
        self.wake.clone()
    }

    pub async fn healthy(&self) -> Result<(), StoreError> {
        self.call(|db| {
            let _: i64 = db
                .conn
                .query_row("SELECT singleton FROM core_identity", [], |row| row.get(0))?;
            Ok(())
        })
        .await
    }

    async fn call<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Database) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        let (reply, result) = oneshot::channel();
        let deadline = Instant::now() + WRITE_WINDOW;
        let job = Box::new(move |db: &mut Database| {
            let outcome = if Instant::now() >= deadline {
                Err(StoreError::Deadline)
            } else {
                operation(db)
            };
            let _ = reply.send(outcome);
        });
        match self
            .worker
            .requests
            .as_ref()
            .ok_or(StoreError::Unavailable)?
            .try_send(job)
        {
            Ok(()) => result.await.map_err(|_| StoreError::Unavailable)?,
            Err(TrySendError::Full(_)) => Err(StoreError::Busy),
            Err(TrySendError::Disconnected(_)) => Err(StoreError::Unavailable),
        }
    }

    pub async fn create(&self, new: NewIncident, now: UnixMillis) -> Result<Incident, StoreError> {
        self.call(move |db| db.create(new, now)).await
    }

    pub async fn create_hazard(
        &self,
        new: NewIncident,
        now: UnixMillis,
    ) -> Result<Incident, StoreError> {
        self.call(move |db| db.create_hazard(new, now)).await
    }

    pub async fn observe_hazard(
        &self,
        new: NewIncident,
        now: UnixMillis,
    ) -> Result<Incident, StoreError> {
        self.call(move |db| db.observe_hazard(new, now)).await
    }

    pub async fn record_recovery(
        &self,
        incident: Uuid,
        scope: SourceScope,
        source_us: i64,
        now: UnixMillis,
    ) -> Result<(), StoreError> {
        self.call(move |db| db.record_recovery(incident, scope, source_us, now))
            .await
    }

    pub async fn get(&self, id: Uuid) -> Result<Incident, StoreError> {
        self.call(move |db| db.get(id)).await
    }

    pub async fn history(
        &self,
        after: Option<HistoryCursor>,
        limit: usize,
    ) -> Result<IncidentPage, StoreError> {
        self.history_visible(after, limit, None).await
    }

    pub async fn history_visible(
        &self,
        after: Option<HistoryCursor>,
        limit: usize,
        allowed: Option<Vec<Uuid>>,
    ) -> Result<IncidentPage, StoreError> {
        self.call(move |db| db.history(after, limit, allowed)).await
    }

    pub async fn snapshot(&self) -> Result<Snapshot, StoreError> {
        self.snapshot_visible(None).await
    }

    pub async fn snapshot_visible(
        &self,
        allowed: Option<Vec<Uuid>>,
    ) -> Result<Snapshot, StoreError> {
        self.call(move |db| db.snapshot(allowed)).await
    }

    pub async fn changes(
        &self,
        after: ChangeCursor,
        limit: usize,
    ) -> Result<Vec<Change>, StoreError> {
        self.call(move |db| db.changes(after, limit)).await
    }

    pub async fn apply(
        &self,
        command: OperatorCommand,
        now: UnixMillis,
    ) -> Result<CommandResult, StoreError> {
        self.call(move |db| db.apply(command, now)).await
    }

    pub async fn record_observation(
        &self,
        id: Uuid,
        scope: SourceScope,
        source_us: i64,
        assessment: Assessment,
        now: UnixMillis,
    ) -> Result<Incident, StoreError> {
        self.call(move |db| db.observe(id, scope, source_us, assessment, now))
            .await
    }

    pub async fn record_semantic(
        &self,
        id: Uuid,
        stamp: EvidenceStamp,
        assessment: Assessment,
        clearance: Clearance,
        now: UnixMillis,
    ) -> Result<Incident, StoreError> {
        self.call(move |db| db.semantic(id, stamp, assessment, clearance, now))
            .await
    }

    pub async fn persist_evidence(
        &self,
        write: EvidenceWrite,
        now: UnixMillis,
    ) -> Result<EvidenceOutcome, StoreError> {
        let bytes = write.validate()?;
        let permit = self
            .evidence_inflight
            .clone()
            .try_acquire_many_owned(bytes)
            .map_err(|_| StoreError::Busy)?;
        self.call(move |db| {
            let _permit = permit;
            db.persist_evidence(write, now)
        })
        .await
    }

    pub async fn read_evidence(
        &self,
        incident: Uuid,
        packet: Uuid,
        ordinal: u8,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        self.call(move |db| db.read_evidence(incident, packet, ordinal))
            .await
    }

    pub async fn evidence_unavailable(
        &self,
        incident: Uuid,
        now: UnixMillis,
    ) -> Result<(), StoreError> {
        self.call(move |db| db.evidence_unavailable(incident, now))
            .await
    }

    pub async fn evidence_packet(
        &self,
        incident: Uuid,
        packet: Uuid,
    ) -> Result<sw_protocol::EvidencePacket, StoreError> {
        self.call(move |db| db.evidence_packet(incident, packet))
            .await
    }

    pub async fn retire_evidence(
        &self,
        cutoff: UnixMillis,
        now: UnixMillis,
    ) -> Result<(), StoreError> {
        if cutoff >= now {
            return Err(StoreError::Invalid);
        }
        self.call(move |db| db.retire_evidence(cutoff, now)).await
    }

    pub async fn backup(&self, destination: PathBuf) -> Result<(), StoreError> {
        self.call(move |db| db.backup(&destination)).await
    }
}
