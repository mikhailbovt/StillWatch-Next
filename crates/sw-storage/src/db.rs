use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions, TryLockError},
    path::{Path, PathBuf},
};

use rusqlite::{
    Connection, ErrorCode, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sw_domain::{
    Assessment, Attention, Box2d, Clearance, EvidenceStamp, Incident, NewIncident, SceneSignature,
    SourceScope, Transition, UncertainIncidentLink, UnixMillis,
};
use tokio::sync::watch;
use uuid::Uuid;

use crate::{
    Change, ChangeCursor, CommandResult, HistoryCursor, Identity, IncidentPage, MAX_CHANGES,
    MAX_PAGE, MAX_SNAPSHOT, OperatorCommand, Snapshot, StoreConfig, StoreError,
    budget::{BUSY_TIMEOUT, DatabaseBudget},
    cameras::camera_filter,
};

pub const DATABASE_FILE: &str = "stillwatch.sqlite3";
pub const APPLICATION_ID: i64 = 0x53574e32;
pub const SCHEMA_VERSION: i64 = 14;
pub(crate) const IDEMPOTENCY_TTL: i64 = 7 * 24 * 60 * 60 * 1_000;
const ALERT_TTL: i64 = 60 * 60 * 1_000;

pub(crate) struct Database {
    pub(crate) conn: Connection,
    pub(crate) budget: DatabaseBudget,
    pub(crate) identity: Identity,
    pub(crate) blobs: PathBuf,
    pub(crate) quota: u64,
    pub(crate) evidence_monotonic: HashMap<Uuid, crate::ObservationTime>,
    pub(crate) blob_bytes: Option<u64>,
    pub(crate) blob_recount: Option<crate::evidence::BlobRecount>,
    evidence_recovery_pending: bool,
    pub(crate) notify: watch::Sender<()>,
    _lock: File,
}

struct HazardObservation {
    time: crate::ObservationTime,
    facts: Option<String>,
    geometry: Option<(SceneSignature, Box2d, bool)>,
}

impl Database {
    pub fn open(
        root: &Path,
        config: &StoreConfig,
        notify: watch::Sender<()>,
    ) -> Result<Self, StoreError> {
        private_dir(root)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("instance.lock"))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(StoreError::InUse),
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }
        let blobs = root.join("blobs");
        private_dir(&blobs)?;
        let file = root.join(DATABASE_FILE);
        let mut conn = Connection::open_with_flags(
            &file,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        migrate(&mut conn)?;
        let journal: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        if !journal.eq_ignore_ascii_case("wal") {
            return Err(StoreError::Schema);
        }
        conn.execute_batch(
            "PRAGMA synchronous = FULL; PRAGMA foreign_keys = ON; PRAGMA trusted_schema = OFF;",
        )?;
        let budget = DatabaseBudget::configure(&conn, &file, config.database_quota_bytes)?;
        let identity = read_identity(&conn)?;
        let mut db = Self {
            conn,
            budget,
            identity,
            blobs,
            quota: config.evidence_quota_bytes,
            evidence_monotonic: HashMap::new(),
            blob_bytes: None,
            blob_recount: None,
            evidence_recovery_pending: false,
            notify,
            _lock: lock,
        };
        if let Err(error) = db.recover_semantic_jobs()
            && !matches!(&error, StoreError::Sql(error) if error.sqlite_error_code() == Some(ErrorCode::DiskFull))
        {
            return Err(error);
        }
        if let Err(error) = db.reconcile_evidence() {
            if !matches!(&error, StoreError::Sql(error)
                if error.sqlite_error_code() == Some(ErrorCode::DiskFull))
            {
                return Err(error);
            }
            // Keep history readable while the administrator restores write headroom.
            db.evidence_recovery_pending = true;
            eprintln!("StillWatch could not finish evidence recovery because the database is full");
        }
        db.budget.check_wal(&db.conn)?;
        Ok(db)
    }

    pub(crate) fn verify_readable(&self) -> Result<(), StoreError> {
        self.conn
            .query_row("SELECT singleton FROM core_identity", [], |_| Ok(()))?;
        Ok(())
    }

    pub(crate) fn ready(&self) -> Result<(), StoreError> {
        self.verify_readable()?;
        self.require_evidence_recovery()?;
        self.budget.ready(&self.conn)
    }

    pub(crate) fn reconcile_storage(&mut self) -> Result<bool, StoreError> {
        self.budget.check_wal(&self.conn)?;
        let mut low = self.budget.is_low(&self.conn)?;
        if self.evidence_recovery_pending && !low && self.budget.writable() {
            if let Err(error) = self.reconcile_evidence() {
                return match self.budget.classify(error) {
                    StoreError::DatabaseCapacity => Ok(true),
                    error => Err(error),
                };
            }
            self.evidence_recovery_pending = false;
            low = self.budget.is_low(&self.conn)?;
        }
        Ok(low || self.evidence_recovery_pending)
    }

    pub fn create(&mut self, new: NewIncident, now: UnixMillis) -> Result<Incident, StoreError> {
        self.create_checked(new, now, false, None)
    }

    pub fn create_hazard(
        &mut self,
        new: NewIncident,
        now: UnixMillis,
    ) -> Result<Incident, StoreError> {
        self.require_evidence_recovery()?;
        self.create_checked(new, now, true, None)
    }

    pub fn create_observed_hazard(
        &mut self,
        new: NewIncident,
        observed: crate::ObservationTime,
        facts: Option<String>,
    ) -> Result<Incident, StoreError> {
        self.require_evidence_recovery()?;
        if new.observed_host_utc != Some(observed.observed_host_utc) {
            return Err(StoreError::Invalid);
        }
        self.create_checked(
            new,
            crate::utc_ms(),
            true,
            Some(HazardObservation {
                time: observed,
                facts,
                geometry: None,
            }),
        )
    }

    pub fn create_observed_hazard_with_geometry(
        &mut self,
        new: NewIncident,
        observed: crate::ObservationTime,
        facts: String,
        geometry: Option<(SceneSignature, Box2d, bool)>,
    ) -> Result<Incident, StoreError> {
        self.require_evidence_recovery()?;
        if new.observed_host_utc != Some(observed.observed_host_utc) {
            return Err(StoreError::Invalid);
        }
        self.create_checked(
            new,
            crate::utc_ms(),
            true,
            Some(HazardObservation {
                time: observed,
                facts: Some(facts),
                geometry,
            }),
        )
    }

    pub(crate) fn require_evidence_recovery(&self) -> Result<(), StoreError> {
        if self.evidence_recovery_pending {
            return Err(StoreError::DatabaseCapacity);
        }
        Ok(())
    }

    fn create_checked(
        &mut self,
        new: NewIncident,
        now: UnixMillis,
        verify_camera: bool,
        observed: Option<HazardObservation>,
    ) -> Result<Incident, StoreError> {
        let hash = digest(&new)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                "SELECT creation_hash, state FROM incidents WHERE id = ?",
                [id(&new.id)],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        if let Some((creation_hash, state)) = existing {
            if creation_hash != hash {
                return Err(StoreError::IncidentConflict);
            }
            return Ok(serde_json::from_str(&state)?);
        }
        if verify_camera {
            verify_camera_scope(&tx, new.scope)?;
        }
        let (now, delayed) = observed.as_ref().map_or((now, false), |observation| {
            let now = crate::utc_ms();
            (
                now,
                !observation
                    .time
                    .evidence_fresh(now, std::time::Instant::now()),
            )
        });
        let mut new = new;
        if delayed && new.assessment == Assessment::ImmediateVisibleConcern {
            new.assessment = Assessment::Concern;
        }
        let mut transition = Incident::create(new, now)?;
        transition.incident.opening_delayed = delayed;
        if !delayed
            && transition.incident.live
            && let Some(observation) = observed.as_ref()
            && let Some((scene, region, true)) = observation.geometry
        {
            transition.incident.possible_continuation_of = recent_compatible_hazard(
                &tx,
                &transition.incident,
                (scene, region),
                observation.time.observed_host_utc,
            )?;
        }
        let incident = &transition.incident;
        let state = serde_json::to_string(incident)?;
        tx.execute(
            "INSERT INTO incidents(id, camera_id, revision, created_at, updated_at, resolved, attention, creation_hash, state, camera_hazard)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![id(&incident.id), id(&incident.scope.camera_id), sql_counter(incident.revision)?, incident.created_at,
                incident.updated_at, incident.resolution.is_some(), attention(incident.attention), hash.as_slice(), state, verify_camera],
        )?;
        if let Some(HazardObservation {
            time,
            facts: Some(facts),
            geometry,
        }) = observed
        {
            append_hazard_observation(
                &tx,
                incident,
                time,
                facts,
                geometry.map(|(signature, _, _)| signature),
            )?;
        }
        append_change(&tx, &transition, true, &state)?;
        tx.commit()?;
        self.notify.send_replace(());
        Ok(transition.incident)
    }

    pub fn get(&self, incident: Uuid) -> Result<Incident, StoreError> {
        get_incident(&self.conn, incident)
    }

    pub(crate) fn resolved_hazards(&self, incidents: &[Uuid]) -> Result<Vec<Uuid>, StoreError> {
        if incidents.len() > 256 {
            return Err(StoreError::Invalid);
        }
        let mut find = self
            .conn
            .prepare("SELECT resolved FROM incidents WHERE id = ? AND camera_hazard = 1")?;
        let mut resolved = Vec::new();
        for &incident in incidents {
            if find
                .query_row([id(&incident)], |row| row.get::<_, bool>(0))
                .optional()?
                .unwrap_or(false)
            {
                resolved.push(incident);
            }
        }
        Ok(resolved)
    }

    pub fn history(
        &self,
        after: Option<HistoryCursor>,
        limit: usize,
        allowed: Option<Vec<Uuid>>,
    ) -> Result<IncidentPage, StoreError> {
        validate_page(limit)?;
        let allowed = camera_filter(allowed)?;
        let incidents = self
            .conn
            .prepare(
                "SELECT state FROM incidents WHERE (?1 IS NULL OR (created_at, id) < (?1, ?2))
             AND (?3 IS NULL OR lower(hex(camera_id)) IN (SELECT value FROM json_each(?3)))
             ORDER BY created_at DESC, id DESC LIMIT ?4",
            )?
            .query_map(
                params![
                    after.map(|cursor| cursor.created_at),
                    after.as_ref().map(|cursor| id(&cursor.id)),
                    allowed,
                    limit as i64 + 1
                ],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let more = incidents.len() > limit;
        let incidents = incidents
            .iter()
            .take(limit)
            .map(|state| serde_json::from_str::<Incident>(state))
            .collect::<Result<Vec<_>, _>>()?;
        let next = incidents
            .last()
            .filter(|_| more)
            .map(|incident| HistoryCursor {
                created_at: incident.created_at,
                id: incident.id,
            });
        Ok(IncidentPage { incidents, next })
    }

    pub fn snapshot(&mut self, allowed: Option<Vec<Uuid>>) -> Result<Snapshot, StoreError> {
        let allowed = camera_filter(allowed)?;
        let tx = self.conn.transaction()?;
        let sequence = seq_head(&tx)?;
        let mut incidents = {
            let mut statement = tx.prepare(
                "SELECT state FROM incidents WHERE resolved = 0
                 AND (?1 IS NULL OR lower(hex(camera_id)) IN (SELECT value FROM json_each(?1)))
                 ORDER BY created_at DESC, id DESC LIMIT ?2",
            )?;
            let states = statement.query_map(params![allowed, MAX_SNAPSHOT as i64 + 1], |row| {
                row.get::<_, String>(0)
            })?;
            states
                .map(|state| Ok(serde_json::from_str::<Incident>(&state?)?))
                .collect::<Result<Vec<_>, StoreError>>()?
        };
        let has_older_open_incidents = incidents.len() > MAX_SNAPSHOT;
        if has_older_open_incidents {
            incidents.truncate(MAX_SNAPSHOT);
        }
        tx.commit()?;
        Ok(Snapshot {
            cursor: ChangeCursor {
                generation: self.identity.generation,
                sequence,
            },
            incidents,
            has_older_open_incidents,
        })
    }

    pub fn change_head(&self) -> Result<ChangeCursor, StoreError> {
        Ok(ChangeCursor {
            generation: self.identity.generation,
            sequence: seq_head(&self.conn)?,
        })
    }

    pub fn changes(
        &mut self,
        after: ChangeCursor,
        limit: usize,
    ) -> Result<Vec<Change>, StoreError> {
        validate_page(limit)?;
        if after.generation != self.identity.generation {
            return Err(StoreError::CursorExpired);
        }
        let tx = self.conn.transaction()?;
        let (oldest, newest) = bounds(&tx)?;
        if after.sequence > newest || after.sequence < oldest {
            return Err(StoreError::CursorExpired);
        }
        let changes = {
            let mut statement = tx.prepare("SELECT sequence, opened, incident FROM change_events WHERE sequence > ? ORDER BY sequence LIMIT ?")?;
            let rows = statement.query_map(
                params![sql_counter(after.sequence)?, limit as i64],
                |row| {
                    Ok((
                        row_counter(row, 0)?,
                        row.get::<_, bool>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )?;
            rows.map(|row| {
                let (sequence, opened, state) = row?;
                let incident = serde_json::from_str(&state)?;
                Ok(Change {
                    cursor: ChangeCursor { sequence, ..after },
                    opened,
                    incident,
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?
        };
        tx.commit()?;
        Ok(changes)
    }

    pub fn apply(
        &mut self,
        command: OperatorCommand,
        now: UnixMillis,
    ) -> Result<CommandResult, StoreError> {
        let key = &command.idempotency_key;
        if !(1..=128).contains(&key.len())
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err(StoreError::Invalid);
        }
        let operation = command.action.operation();
        let actor = id(&command.actor_id);
        let hash = digest(&(
            command.incident_id,
            command.expected_revision,
            &command.action,
        ))?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM idempotency_records WHERE actor_id = ? AND operation = ? AND key = ? AND expires_at <= ?",
            params![actor, operation, key, now],
        )?;
        let cached = tx.query_row(
            "SELECT request_hash, result FROM idempotency_records WHERE actor_id = ? AND operation = ? AND key = ?",
            params![actor, operation, key], |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
        ).optional()?;
        if let Some((previous_hash, state)) = cached {
            if previous_hash != hash {
                return Err(StoreError::Idempotency);
            }
            return Ok(CommandResult {
                incident: serde_json::from_str(&state)?,
                replayed: true,
            });
        }
        let incident = get_incident(&tx, command.incident_id)?;
        if incident.revision != command.expected_revision {
            return Err(StoreError::Revision {
                current: incident.revision,
            });
        }
        let transition = incident.apply(command.actor_id, &command.action, now)?;
        let (sequence, state) = update_incident(&tx, &transition)?;
        tx.execute(
            "INSERT INTO operator_actions(change_sequence, incident_id, incident_revision, actor_id, occurred_at, action) VALUES (?, ?, ?, ?, ?, ?)",
            params![sequence, id(&incident.id), sql_counter(transition.incident.revision)?, actor, transition.incident.updated_at, serde_json::to_string(&command.action)?],
        )?;
        tx.execute(
            "INSERT INTO idempotency_records(actor_id, operation, key, request_hash, result, expires_at) VALUES (?, ?, ?, ?, ?, ?)",
            params![actor, operation, key, hash.as_slice(), state, now.saturating_add(IDEMPOTENCY_TTL)],
        )?;
        tx.commit()?;
        self.notify.send_replace(());
        Ok(CommandResult {
            incident: transition.incident,
            replayed: false,
        })
    }

    pub fn observe(
        &mut self,
        incident: Uuid,
        scope: SourceScope,
        source_us: i64,
        assessment: Assessment,
        now: UnixMillis,
    ) -> Result<Incident, StoreError> {
        self.transition(incident, |current| {
            current.record_observation(scope, source_us, assessment, now)
        })
    }

    pub fn observe_hazard(
        &mut self,
        new: NewIncident,
        now: UnixMillis,
    ) -> Result<Incident, StoreError> {
        self.observe_hazard_checked(new, now, None)
    }

    pub fn observe_hazard_with_facts(
        &mut self,
        new: NewIncident,
        observed: crate::ObservationTime,
        facts: String,
        scene_signature: Option<SceneSignature>,
    ) -> Result<Incident, StoreError> {
        if new.observed_host_utc != Some(observed.observed_host_utc) {
            return Err(StoreError::Invalid);
        }
        self.observe_hazard_checked(
            new,
            crate::utc_ms(),
            Some((observed, facts, scene_signature)),
        )
    }

    fn observe_hazard_checked(
        &mut self,
        mut new: NewIncident,
        supplied: UnixMillis,
        observed: Option<(crate::ObservationTime, String, Option<SceneSignature>)>,
    ) -> Result<Incident, StoreError> {
        self.require_evidence_recovery()?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = get_incident(&tx, new.id)?;
        if current.scope != new.scope
            || current.category != new.category
            || current.analysis_origin != new.analysis_origin
        {
            return Err(sw_domain::DomainError::Stale.into());
        }
        if current.resolution.is_some() || current.last_source_us >= new.last_source_us {
            return Ok(current);
        }
        verify_camera_scope(&tx, new.scope)?;
        let now = observed.as_ref().map_or(supplied, |(time, _, _)| {
            let now = crate::utc_ms();
            if new.assessment == Assessment::ImmediateVisibleConcern
                && !time.evidence_fresh(now, std::time::Instant::now())
            {
                new.assessment = Assessment::Concern;
            }
            now
        });
        let transition =
            current.record_observation(new.scope, new.last_source_us, new.assessment, now)?;
        update_incident(&tx, &transition)?;
        if let Some((time, facts, scene_signature)) = observed {
            append_hazard_observation(&tx, &transition.incident, time, facts, scene_signature)?;
        }
        tx.commit()?;
        self.notify.send_replace(());
        Ok(transition.incident)
    }

    pub fn record_recovery(
        &mut self,
        incident: Uuid,
        scope: SourceScope,
        source_us: i64,
        now: UnixMillis,
    ) -> Result<(), StoreError> {
        self.record_recovery_checked(incident, scope, source_us, now, None)
    }

    pub fn record_observed_recovery(
        &mut self,
        incident: Uuid,
        scope: SourceScope,
        source_us: i64,
        observed: crate::ObservationTime,
    ) -> Result<(), StoreError> {
        self.record_recovery_checked(incident, scope, source_us, 0, Some(observed))
    }

    fn record_recovery_checked(
        &mut self,
        incident: Uuid,
        scope: SourceScope,
        source_us: i64,
        now: UnixMillis,
        observed: Option<crate::ObservationTime>,
    ) -> Result<(), StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        verify_camera_scope(&tx, scope)?;
        let current = get_incident(&tx, incident)?;
        let now = if let Some(observed) = observed {
            let now = crate::utc_ms();
            if !observed.fresh(
                now,
                std::time::Instant::now(),
                std::time::Duration::from_secs(3),
            ) {
                return Err(sw_domain::DomainError::Stale.into());
            }
            now
        } else {
            now
        };
        let transition = current.record_recovery(scope, source_us, now)?;
        update_incident(&tx, &transition)?;
        tx.commit()?;
        self.notify.send_replace(());
        Ok(())
    }

    pub fn semantic(
        &mut self,
        incident: Uuid,
        stamp: EvidenceStamp,
        assessment: Assessment,
        clearance: Clearance,
        now: UnixMillis,
    ) -> Result<Incident, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = get_incident(&tx, incident)?;
        if tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM cameras WHERE id = ?)",
            [id(&stamp.scope.camera_id)],
            |row| row.get::<_, bool>(0),
        )? {
            verify_camera_scope(&tx, stamp.scope)?;
            if !self
                .evidence_monotonic
                .get(&stamp.packet_id)
                .is_some_and(|time| time.evidence_fresh(crate::utc_ms(), std::time::Instant::now()))
            {
                return Err(sw_domain::DomainError::Stale.into());
            }
        }
        let transition = current.record_semantic(stamp, assessment, clearance, now)?;
        update_incident(&tx, &transition)?;
        tx.commit()?;
        self.notify.send_replace(());
        Ok(transition.incident)
    }

    fn transition(
        &mut self,
        incident: Uuid,
        reducer: impl FnOnce(Incident) -> Result<Transition, sw_domain::DomainError>,
    ) -> Result<Incident, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let transition = reducer(get_incident(&tx, incident)?)?;
        update_incident(&tx, &transition)?;
        tx.commit()?;
        self.notify.send_replace(());
        Ok(transition.incident)
    }
}

pub(crate) fn verify_camera_scope(conn: &Connection, scope: SourceScope) -> Result<(), StoreError> {
    let current = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM cameras WHERE id = ? AND source_epoch = ? AND scene_revision = ? AND enabled = 1 AND deleted = 0)",
        params![id(&scope.camera_id), sql_counter(scope.epoch)?, sql_counter(scope.scene_revision)?], |r| r.get::<_, bool>(0),
    )?;
    if current {
        Ok(())
    } else {
        Err(sw_domain::DomainError::Stale.into())
    }
}

fn recent_compatible_hazard(
    conn: &Connection,
    incident: &Incident,
    (signature, region): (SceneSignature, Box2d),
    now: UnixMillis,
) -> Result<Option<UncertainIncidentLink>, StoreError> {
    const ASSOCIATION_WINDOW_MS: i64 = 60_000;
    const MAX_CANDIDATES: i64 = 256;
    let scope = incident.scope;
    let oldest = now.saturating_sub(ASSOCIATION_WINDOW_MS);
    let mut query = conn.prepare(
        "SELECT i.id, json_extract(i.state, '$.scope.epoch'), h.facts, h.scene_signature
         FROM incidents i
         JOIN hazard_observations h ON h.incident_id = i.id
           AND h.source_us = (SELECT max(o.source_us) FROM hazard_observations o
                              WHERE o.incident_id = i.id AND o.scene_signature IS NOT NULL)
         WHERE i.camera_id = ?1 AND i.resolved = 0 AND i.camera_hazard = 1
           AND i.updated_at >= ?2 AND h.observed_host_utc BETWEEN ?2 AND ?3
           AND json_extract(i.state, '$.scope.scene_revision') = ?4
           AND json_extract(i.state, '$.scope.epoch') < ?5
           AND json_extract(i.state, '$.category') = ?6 AND json_extract(i.state, '$.live') = 1
         ORDER BY i.updated_at DESC, i.id LIMIT ?7",
    )?;
    let category = match incident.category {
        sw_domain::Category::Fire => "fire",
        sw_domain::Category::Medical => "medical",
        sw_domain::Category::Overview => return Ok(None),
    };
    let mut candidates = query.query(params![
        id(&scope.camera_id),
        oldest,
        now,
        sql_counter(scope.scene_revision)?,
        sql_counter(scope.epoch)?,
        category,
        MAX_CANDIDATES + 1
    ])?;
    let mut possible = None;
    let mut checked = 0;
    while let Some(row) = candidates.next()? {
        checked += 1;
        if checked > MAX_CANDIDATES {
            return Ok(None);
        }
        if !signature.matches(&row.get::<_, Vec<u8>>(3)?) {
            continue;
        }
        let (_, previous): (sw_protocol::LocalFacts, Box2d) =
            serde_json::from_str(&row.get::<_, String>(2)?)?;
        if region.iou(previous) < 0.35 {
            continue;
        }
        if possible.is_some() {
            return Ok(None);
        }
        let bytes = row.get::<_, Vec<u8>>(0)?;
        possible = Some(UncertainIncidentLink {
            incident_id: Uuid::from_slice(&bytes).map_err(|_| StoreError::Schema)?,
            source_epoch: row_counter(row, 1)?,
        });
    }
    Ok(possible)
}

fn append_hazard_observation(
    conn: &Connection,
    incident: &Incident,
    observed: crate::ObservationTime,
    facts: String,
    signature: Option<SceneSignature>,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO hazard_observations(incident_id, source_us, observed_host_utc, facts, scene_signature)
         VALUES (?, ?, ?, ?, ?)",
        params![
            id(&incident.id),
            incident.last_source_us,
            observed.observed_host_utc,
            facts,
            signature.as_ref().map(|signature| signature.0.as_slice())
        ],
    )?;
    conn.execute(
        "DELETE FROM hazard_observations WHERE incident_id = ?1 AND source_us < (
           SELECT source_us FROM hazard_observations WHERE incident_id = ?1
           ORDER BY source_us DESC LIMIT 1 OFFSET 63)",
        [id(&incident.id)],
    )?;
    Ok(())
}

pub(crate) fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    let application_id: i64 = conn.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    match (application_id, version) {
        (APPLICATION_ID, SCHEMA_VERSION) => Ok(()),
        (APPLICATION_ID, 1..=13) => {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if version == 1 {
                tx.execute_batch(include_str!("migrations/002.sql"))?;
            }
            if version <= 2 {
                tx.execute_batch(include_str!("migrations/003.sql"))?;
            }
            if version <= 3 {
                tx.execute_batch(include_str!("migrations/004.sql"))?;
            }
            if version <= 4 {
                tx.execute_batch(include_str!("migrations/005.sql"))?;
            }
            if version <= 5 {
                tx.execute_batch(include_str!("migrations/006.sql"))?;
            }
            if version <= 6 {
                tx.execute_batch(include_str!("migrations/007.sql"))?;
            }
            if version <= 7 {
                tx.execute_batch(include_str!("migrations/008.sql"))?;
            }
            if version <= 8 {
                tx.execute_batch(include_str!("migrations/009.sql"))?;
            }
            if version <= 9 {
                tx.execute_batch(include_str!("migrations/010.sql"))?;
            }
            if version <= 10 {
                tx.execute_batch(include_str!("migrations/011.sql"))?;
            }
            if version <= 11 {
                tx.execute_batch(include_str!("migrations/012.sql"))?;
            }
            if version <= 12 {
                tx.execute_batch(include_str!("migrations/013.sql"))?;
            }
            tx.execute_batch(include_str!("migrations/014.sql"))?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
            Ok(())
        }
        (0, 0) => {
            let tables: i64 = conn.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type = 'table'",
                [],
                |row| row.get(0),
            )?;
            if tables != 0 {
                return Err(StoreError::Schema);
            }
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(include_str!("migrations/001.sql"))?;
            tx.execute_batch(include_str!("migrations/002.sql"))?;
            tx.execute_batch(include_str!("migrations/003.sql"))?;
            tx.execute_batch(include_str!("migrations/004.sql"))?;
            tx.execute_batch(include_str!("migrations/005.sql"))?;
            tx.execute_batch(include_str!("migrations/006.sql"))?;
            tx.execute_batch(include_str!("migrations/007.sql"))?;
            tx.execute_batch(include_str!("migrations/008.sql"))?;
            tx.execute_batch(include_str!("migrations/009.sql"))?;
            tx.execute_batch(include_str!("migrations/010.sql"))?;
            tx.execute_batch(include_str!("migrations/011.sql"))?;
            tx.execute_batch(include_str!("migrations/012.sql"))?;
            tx.execute_batch(include_str!("migrations/013.sql"))?;
            tx.execute_batch(include_str!("migrations/014.sql"))?;
            tx.execute(
                "INSERT INTO core_identity (singleton, instance_id, stream_generation) VALUES (1, ?, ?)",
                params![id(&Uuid::new_v4()), id(&Uuid::new_v4())],
            )?;
            tx.pragma_update(None, "application_id", APPLICATION_ID)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
            Ok(())
        }
        _ => Err(StoreError::Schema),
    }
}

pub(crate) fn read_identity(conn: &Connection) -> Result<Identity, StoreError> {
    let (instance, generation) = conn.query_row(
        "SELECT instance_id, stream_generation FROM core_identity WHERE singleton = 1",
        [],
        |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
    )?;
    Ok(Identity {
        instance_id: Uuid::from_slice(&instance).map_err(|_| StoreError::Schema)?,
        generation: Uuid::from_slice(&generation).map_err(|_| StoreError::Schema)?,
    })
}

pub(crate) fn private_dir(path: &Path) -> Result<(), StoreError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(StoreError::Invalid);
    }
    Ok(())
}

pub(crate) fn get_incident(conn: &Connection, incident: Uuid) -> Result<Incident, StoreError> {
    let state: String = conn
        .query_row(
            "SELECT state FROM incidents WHERE id = ?",
            [id(&incident)],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(StoreError::NotFound)?;
    Ok(serde_json::from_str(&state)?)
}

pub(crate) fn update_incident(
    tx: &Transaction<'_>,
    transition: &Transition,
) -> Result<(i64, String), StoreError> {
    let incident = &transition.incident;
    let state = serde_json::to_string(incident)?;
    let revision = sql_counter(incident.revision)?;
    let written = tx.execute(
        "UPDATE incidents SET revision = ?, updated_at = ?, resolved = ?, attention = ?, state = ? WHERE id = ? AND revision = ?",
        params![revision, incident.updated_at, incident.resolution.is_some(), attention(incident.attention), state, id(&incident.id), revision - 1],
    )?;
    if written != 1 {
        return Err(StoreError::Invalid);
    }
    let sequence = append_change(tx, transition, false, &state)?;
    Ok((sequence, state))
}

pub(crate) fn append_change(
    tx: &Transaction<'_>,
    transition: &Transition,
    opened: bool,
    state: &str,
) -> Result<i64, StoreError> {
    let incident = &transition.incident;
    let incident_id = id(&incident.id);
    let cycle = sql_counter(incident.attention_cycle)?;
    tx.execute(
        "INSERT INTO change_events(incident_id, revision, occurred_at, opened, incident) VALUES (?, ?, ?, ?, ?)",
        params![incident_id, sql_counter(incident.revision)?, incident.updated_at, opened, state],
    )?;
    let sequence = tx.last_insert_rowid();
    if incident.resolution.is_some() || transition.notification.is_some() {
        tx.execute("UPDATE notification_outbox SET state = 'expired' WHERE incident_id = ? AND state = 'queued'", [incident_id])?;
        tx.execute(
            "DELETE FROM alert_silences WHERE incident_id = ?",
            [incident_id],
        )?;
    }
    if let Some(level) = transition.notification {
        tx.execute(
            "INSERT INTO notification_outbox(incident_id, attention_cycle, attention, state, created_at, expires_at, next_attempt_at)
             VALUES (?, ?, ?, 'queued', ?, ?, ?)",
            params![incident_id, cycle, attention(level), incident.updated_at, incident.updated_at.saturating_add(ALERT_TTL), incident.updated_at],
        )?;
    }
    if let Some(silence) = transition.silence {
        tx.execute(
            "INSERT INTO alert_silences VALUES (?, ?, ?, ?) ON CONFLICT(incident_id, device_id) DO UPDATE SET attention_cycle = excluded.attention_cycle, until_utc = excluded.until_utc",
            params![incident_id, id(&silence.device_id), sql_counter(silence.attention_cycle)?, silence.until],
        )?;
    }
    tx.execute(
        "DELETE FROM change_events WHERE sequence <= ?",
        [sequence - MAX_CHANGES as i64],
    )?;
    Ok(sequence)
}

fn bounds(conn: &Connection) -> Result<(u64, u64), StoreError> {
    Ok(conn.query_row(
        "SELECT max(change_floor, coalesce((SELECT min(sequence)-1 FROM change_events), 0)),
                coalesce((SELECT seq FROM sqlite_sequence WHERE name = 'change_events'), 0)
         FROM core_identity WHERE singleton = 1",
        [],
        |row| Ok((row_counter(row, 0)?, row_counter(row, 1)?)),
    )?)
}

fn seq_head(conn: &Connection) -> Result<u64, StoreError> {
    Ok(conn.query_row(
        "SELECT coalesce((SELECT seq FROM sqlite_sequence WHERE name = 'change_events'), 0)",
        [],
        |row| row_counter(row, 0),
    )?)
}

pub(crate) fn id(id: &Uuid) -> &[u8] {
    id.as_bytes()
}

pub(crate) fn sql_counter(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::Invalid)
}

pub(crate) fn row_counter(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<u64> {
    u64::try_from(row.get::<_, i64>(column)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn attention(level: Attention) -> i64 {
    match level {
        Attention::Info => 0,
        Attention::Review => 1,
        Attention::Urgent => 2,
    }
}

fn validate_page(limit: usize) -> Result<(), StoreError> {
    if (1..=MAX_PAGE).contains(&limit) {
        Ok(())
    } else {
        Err(StoreError::Invalid)
    }
}

fn digest(value: &impl Serialize) -> Result<[u8; 32], StoreError> {
    Ok(Sha256::digest(serde_json::to_vec(value)?).into())
}
