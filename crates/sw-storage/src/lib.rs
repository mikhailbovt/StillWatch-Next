#![forbid(unsafe_code)]

mod alerts;
mod backup;
mod budget;
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
        Arc, Mutex,
        mpsc::{self, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use sw_domain::{
    Assessment, Clearance, DomainError, EvidenceStamp, Incident, NewIncident, OperatorAction,
    SourceScope, UnixMillis,
};
use thiserror::Error;
use tokio::sync::{Semaphore, oneshot, watch};
use uuid::Uuid;

use db::Database;

pub use backup::{BackupLimits, restore_backup, restore_backup_with_limits};
pub use budget::DEFAULT_DATABASE_QUOTA_BYTES;
pub use cameras::{CameraAction, CameraCommand, CameraLimits, CameraRecord, CameraResult};
pub use evidence::{
    EvidenceFailure, EvidenceImage, EvidenceOutcome, EvidenceResult, EvidenceWrite, ObservationTime,
};
pub use health::{HealthIssue, HealthObservation};
pub use legacy::{LegacyImport, LegacyManifest, import_legacy};
pub use semantic::{
    SemanticClaim, SemanticCompleted, SemanticOutcome, SemanticSchedule, SemanticWork,
};
pub use trust::{DeviceAccess, PairingGrant};

const MAX_PAGE: usize = 200;
const MAX_CHANGES: usize = 100_000;
const MAX_SNAPSHOT: usize = 2_000;
const WRITE_WINDOW: Duration = Duration::from_secs(10);
const COMPLETION_BUDGET: Duration = Duration::from_secs(15);
const WRITER_RESTART_BUDGET: Duration = Duration::from_secs(60);

pub fn utc_ms() -> UnixMillis {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(time) => time.as_millis() as UnixMillis,
        Err(time) => -(time.duration().as_millis() as UnixMillis),
    }
}

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
    #[error("database request exceeded its completion deadline and may still commit")]
    OutcomeUnknown,
    #[error("database writer is unavailable. Retry writes with the original idempotency key")]
    Unavailable,
    #[error("database storage capacity reached")]
    DatabaseCapacity,
    #[error("backup exceeds its byte budget or available destination space")]
    BackupCapacity,
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
    pub has_older_open_incidents: bool,
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
    pub database_quota_bytes: u64,
    pub queue_capacity: usize,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            evidence_quota_bytes: 500 * 1024 * 1024,
            database_quota_bytes: DEFAULT_DATABASE_QUOTA_BYTES,
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

#[derive(Default)]
struct WriterProgress {
    running_since: Option<Instant>,
    oldest_queued_at: std::collections::VecDeque<Instant>,
    queue_high_water: usize,
    maximum_queue_wait_ms: u64,
    maximum_operation_ms: u64,
    completed: u64,
    cancelled_queued: u64,
    rejected_queue_full: u64,
}

#[derive(Clone)]
pub struct Store {
    worker: Arc<Worker>,
    identity: Identity,
    wake: watch::Receiver<()>,
    evidence_inflight: Arc<Semaphore>,
    progress: Arc<Mutex<WriterProgress>>,
    queue_capacity: usize,
}

impl Store {
    pub fn open(root: &Path, config: StoreConfig) -> Result<Self, StoreError> {
        if !(1..=4096).contains(&config.queue_capacity)
            || config.evidence_quota_bytes > i64::MAX as u64
            || !budget::valid_quota(config.database_quota_bytes)
        {
            return Err(StoreError::Invalid);
        }
        let root = root.to_owned();
        let queue_capacity = config.queue_capacity;
        let (requests, incoming) = mpsc::sync_channel::<Job>(config.queue_capacity);
        let (boot, startup) = mpsc::sync_channel(1);
        let (notify, wake) = watch::channel(());
        let handle = thread::Builder::new()
            .name("stillwatch-sqlite".into())
            .spawn(move || match Database::open(&root, &config, notify) {
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
            })?;
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
            progress: Arc::default(),
            queue_capacity,
        })
    }

    pub fn identity(&self) -> Identity {
        self.identity
    }

    pub fn subscribe(&self) -> watch::Receiver<()> {
        self.wake.clone()
    }

    pub fn writer_status(&self) -> Option<sw_protocol::WriterStatus> {
        let progress = self.progress.lock().ok()?;
        Some(sw_protocol::WriterStatus {
            queue_capacity: self.queue_capacity,
            queued: progress.oldest_queued_at.len().min(self.queue_capacity),
            queue_high_water: progress.queue_high_water,
            running_ms: progress.running_since.map(|at| counter_ms(at.elapsed())),
            oldest_queued_ms: progress
                .oldest_queued_at
                .front()
                .map(|at| counter_ms(at.elapsed())),
            completed: sw_protocol::Counter(progress.completed),
            rejected_queue_full: sw_protocol::Counter(progress.rejected_queue_full),
            cancelled_queued: sw_protocol::Counter(progress.cancelled_queued),
            maximum_queue_wait_ms: sw_protocol::Counter(progress.maximum_queue_wait_ms),
            maximum_operation_ms: sw_protocol::Counter(progress.maximum_operation_ms),
        })
    }

    pub fn writer_requires_restart(&self) -> bool {
        self.worker
            .handle
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
            || self.progress.lock().map_or(true, |state| {
                state
                    .running_since
                    .is_some_and(|started| started.elapsed() >= WRITER_RESTART_BUDGET)
            })
    }

    pub async fn healthy(&self) -> Result<(), StoreError> {
        self.call(|db| db.verify_readable()).await
    }

    pub async fn ready(&self) -> Result<(), StoreError> {
        self.call(|db| db.ready()).await
    }

    /// Check write headroom and retry any recovery deferred by a full store.
    pub async fn database_budget_low(&self) -> Result<bool, StoreError> {
        self.call(|db| db.reconcile_storage()).await
    }

    async fn call<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Database) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        self.call_with_budget(COMPLETION_BUDGET, operation).await
    }

    async fn call_with_budget<T: Send + 'static>(
        &self,
        completion_budget: Duration,
        operation: impl FnOnce(&mut Database) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        let result = self.enqueue(operation)?;
        tokio::time::timeout(completion_budget, result)
            .await
            .map_err(|_| StoreError::OutcomeUnknown)?
            .map_err(|_| StoreError::Unavailable)?
    }

    fn enqueue<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Database) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<oneshot::Receiver<Result<T, StoreError>>, StoreError> {
        let (reply, result) = oneshot::channel();
        let queued_at = Instant::now();
        let deadline = queued_at + WRITE_WINDOW;
        let progress = Arc::clone(&self.progress);
        let job = Box::new(move |db: &mut Database| {
            let started = Instant::now();
            let not_executed = reply.is_closed() || started >= deadline;
            if let Ok(mut state) = progress.lock() {
                state.oldest_queued_at.pop_front();
                state.maximum_queue_wait_ms = state
                    .maximum_queue_wait_ms
                    .max(counter_ms(started - queued_at).0);
                if not_executed {
                    state.cancelled_queued = state.cancelled_queued.saturating_add(1);
                } else {
                    state.running_since = Some(started);
                }
            }
            let outcome = if not_executed {
                Err(StoreError::Deadline)
            } else {
                if db.budget.should_retry() {
                    let _ = db.budget.check_wal(&db.conn);
                }
                let before = db.conn.total_changes();
                let outcome = operation(db).map_err(|error| db.budget.classify(error));
                if db.conn.total_changes() != before
                    || matches!(
                        &outcome,
                        Err(StoreError::DatabaseCapacity | StoreError::Sql(_))
                    )
                {
                    // A committed result stays valid even if checkpointing fails.
                    let _ = db.budget.check_wal(&db.conn);
                }
                outcome
            };
            if !not_executed && let Ok(mut state) = progress.lock() {
                state.running_since = None;
                state.completed = state.completed.saturating_add(1);
                state.maximum_operation_ms = state
                    .maximum_operation_ms
                    .max(counter_ms(started.elapsed()).0);
            }
            let _ = reply.send(outcome);
        });
        {
            let sender = self
                .worker
                .requests
                .as_ref()
                .ok_or(StoreError::Unavailable)?;
            let mut state = self.progress.lock().map_err(|_| StoreError::Unavailable)?;
            match sender.try_send(job) {
                Ok(()) => {
                    state.oldest_queued_at.push_back(queued_at);
                    state.queue_high_water = state
                        .queue_high_water
                        .max(state.oldest_queued_at.len().min(self.queue_capacity));
                }
                Err(TrySendError::Full(_)) => {
                    state.rejected_queue_full = state.rejected_queue_full.saturating_add(1);
                    return Err(StoreError::Busy);
                }
                Err(TrySendError::Disconnected(_)) => return Err(StoreError::Unavailable),
            }
        }
        Ok(result)
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

    pub async fn create_observed_hazard(
        &self,
        new: NewIncident,
        observed: ObservationTime,
    ) -> Result<Incident, StoreError> {
        self.call(move |db| db.create_observed_hazard(new, observed, None))
            .await
    }

    pub async fn record_hazard_observation(
        &self,
        new: NewIncident,
        observed: ObservationTime,
        facts: sw_protocol::LocalFacts,
        region: sw_domain::Box2d,
        opening: bool,
    ) -> Result<Incident, StoreError> {
        if !region.valid()
            || !matches!(
                (new.category, facts),
                (
                    sw_domain::Category::Fire,
                    sw_protocol::LocalFacts::Fire { .. }
                ) | (
                    sw_domain::Category::Medical,
                    sw_protocol::LocalFacts::Medical { .. }
                )
            )
        {
            return Err(StoreError::Invalid);
        }
        let facts = serde_json::to_string(&(facts, region))?;
        self.call(move |db| {
            if opening {
                db.create_observed_hazard(new, observed, Some(facts))
            } else {
                db.observe_hazard_with_facts(new, observed, facts)
            }
        })
        .await
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

    pub async fn record_fresh_recovery(
        &self,
        incident: Uuid,
        scope: SourceScope,
        source_us: i64,
        observed: ObservationTime,
    ) -> Result<(), StoreError> {
        self.call(move |db| db.record_observed_recovery(incident, scope, source_us, observed))
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

    pub async fn change_head(&self) -> Result<ChangeCursor, StoreError> {
        self.call(|db| db.change_head()).await
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
        self.retire_evidence_step(cutoff, now).await.map(|_| ())
    }

    pub async fn retire_evidence_step(
        &self,
        cutoff: UnixMillis,
        now: UnixMillis,
    ) -> Result<bool, StoreError> {
        if cutoff >= now {
            return Err(StoreError::Invalid);
        }
        self.call(move |db| db.retire_evidence(cutoff, now)).await
    }

    pub async fn backup(&self, destination: PathBuf) -> Result<(), StoreError> {
        self.backup_with_limits(destination, BackupLimits::default())
            .await
    }

    pub async fn backup_with_limits(
        &self,
        destination: PathBuf,
        limits: BackupLimits,
    ) -> Result<(), StoreError> {
        // This entry point runs offline with exclusive ownership of the store.
        // Large, valid backups can take longer than a serving API command.
        self.enqueue(move |db| db.backup(&destination, limits))?
            .await
            .map_err(|_| StoreError::Unavailable)?
    }
}

fn counter_ms(duration: Duration) -> sw_protocol::Counter {
    sw_protocol::Counter(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
}
