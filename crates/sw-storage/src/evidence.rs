use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sw_domain::{EvidenceStamp, EvidenceState, Incident, Transition, UnixMillis};
use sw_protocol::{
    Category, Counter, EvidenceContext, EvidenceImageAvailability, EvidenceImageKind,
    EvidenceImageMetadata, EvidencePacket, LocalFacts, UtcTimestamp,
};
use uuid::Uuid;

use crate::{
    StoreError,
    db::{Database, get_incident, id, row_counter, sql_counter, update_incident},
};

pub(crate) const MAX_IMAGE: usize = 4 * 1024 * 1024;
const MAX_PACKET: usize = 32 * 1024 * 1024;
const LIVE_EVIDENCE_TTL_MS: i64 = 15_000;
const WALL_CLOCK_ERROR_MS: u64 = 1_000;

#[derive(Clone, Copy, Debug)]
pub struct ObservationTime {
    pub observed_host_utc: UnixMillis,
    observed_at: Instant,
}

impl ObservationTime {
    pub fn new(observed_host_utc: UnixMillis, observed_at: Instant) -> Self {
        Self {
            observed_host_utc,
            observed_at,
        }
    }

    pub fn expires_at(self) -> UnixMillis {
        self.observed_host_utc.saturating_add(LIVE_EVIDENCE_TTL_MS)
    }

    pub fn evidence_fresh(self, now: UnixMillis, monotonic: Instant) -> bool {
        self.fresh(
            now,
            monotonic,
            Duration::from_millis(LIVE_EVIDENCE_TTL_MS as u64),
        )
    }

    pub fn fresh(self, now: UnixMillis, monotonic: Instant, max_age: Duration) -> bool {
        let Some(age) = monotonic.checked_duration_since(self.observed_at) else {
            return false;
        };
        let wall_age_ms = now.saturating_sub(self.observed_host_utc);
        age < max_age
            && now < self.expires_at()
            && wall_age_ms >= 0
            && wall_age_ms.abs_diff(age.as_millis() as i64) <= WALL_CLOCK_ERROR_MS
    }
}

pub struct EvidenceImage {
    pub source_us: i64,
    pub kind: EvidenceImageKind,
    pub jpeg: Vec<u8>,
}

pub struct EvidenceWrite {
    pub incident_id: Uuid,
    pub expected_revision: u64,
    pub stamp: EvidenceStamp,
    pub observation_time: Option<ObservationTime>,
    pub context: Option<EvidenceContext>,
    pub images: Vec<EvidenceImage>,
}

impl EvidenceWrite {
    pub(crate) fn validate(&self) -> Result<u32, StoreError> {
        if !(1..=16).contains(&self.images.len())
            || !self
                .images
                .iter()
                .any(|image| image.kind == EvidenceImageKind::Overview)
            || self
                .images
                .windows(2)
                .any(|w| w[0].source_us > w[1].source_us)
        {
            return Err(StoreError::Invalid);
        }
        if let Some(context) = &self.context {
            let interval = context.observed_interval;
            let coverage = context.coverage;
            if !interval.start_source_s.is_finite()
                || !interval.end_source_s.is_finite()
                || interval.end_source_s < interval.start_source_s
                || coverage.expected_samples < coverage.received_samples
                || context.limitations.len() > 8
                || [&context.policy_version, &context.prompt_version]
                    .into_iter()
                    .any(|s| {
                        s.is_empty()
                            || s.len() > 64
                            || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    })
            {
                return Err(StoreError::Invalid);
            }
            let valid = match context.local_facts {
                LocalFacts::Medical {
                    recovery_seconds, ..
                } => recovery_seconds.is_finite() && recovery_seconds >= 0.0,
                LocalFacts::Fire {
                    controlled_region_overlap,
                    detector_confidence,
                    ..
                } => {
                    (0.0..=1.0).contains(&detector_confidence)
                        && controlled_region_overlap
                            .is_none_or(|value| (0.0..=1.0).contains(&value))
                }
            };
            if !valid {
                return Err(StoreError::Invalid);
            }
        }
        if self
            .observation_time
            .is_some_and(|time| time.expires_at() != self.stamp.expires_at)
        {
            return Err(StoreError::Invalid);
        }
        let mut bytes = 0;
        for image in &self.images {
            let data = &image.jpeg;
            if data.len() > MAX_IMAGE
                || !data.starts_with(&[0xff, 0xd8, 0xff])
                || !data.ends_with(&[0xff, 0xd9])
            {
                return Err(StoreError::Invalid);
            }
            bytes += data.len();
        }
        if bytes > MAX_PACKET {
            return Err(StoreError::Invalid);
        }
        Ok(bytes as u32)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceFailure {
    Quota,
    Io,
    Missing,
}

#[derive(Debug, Eq, PartialEq)]
pub struct EvidenceResult {
    pub packet_id: Uuid,
    pub incident_revision: u64,
    pub available: u8,
    pub failures: Vec<(u8, EvidenceFailure)>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum EvidenceOutcome {
    Persisted(EvidenceResult),
    Partial(EvidenceResult),
    Failed(EvidenceResult),
}

impl EvidenceOutcome {
    fn new(result: EvidenceResult) -> Self {
        if result.available == 0 {
            Self::Failed(result)
        } else if result.failures.is_empty() {
            Self::Persisted(result)
        } else {
            Self::Partial(result)
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct BlobInfo {
    pub digest: String,
    pub byte_length: u64,
}

impl Database {
    pub fn persist_evidence(
        &mut self,
        write: EvidenceWrite,
        now: UnixMillis,
    ) -> Result<EvidenceOutcome, StoreError> {
        self.require_evidence_recovery()?;
        let monotonic = Instant::now();
        let now = write.observation_time.map_or(now, |_| crate::utc_ms());
        let image_hashes: Vec<_> = write
            .images
            .iter()
            .map(|image| hex_digest(&image.jpeg))
            .collect();
        let signature: Vec<_> = write
            .images
            .iter()
            .zip(&image_hashes)
            .map(|(image, digest)| (image.source_us, image.kind, digest))
            .collect();
        let (incident_id, revision, stamp, context) = (
            write.incident_id,
            write.expected_revision,
            write.stamp,
            &write.context,
        );
        let request_hash = Sha256::digest(match write.observation_time {
            Some(time) => serde_json::to_vec(&(
                incident_id,
                revision,
                stamp,
                context,
                signature,
                time.observed_host_utc,
            ))?,
            None => serde_json::to_vec(&(incident_id, revision, stamp, context, signature))?,
        });
        let packet = id(&write.stamp.packet_id);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                "SELECT request_hash, incident_revision FROM evidence_packets WHERE id = ?",
                [packet],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row_counter(row, 1)?)),
            )
            .optional()?;
        if let Some((hash, incident_revision)) = existing {
            if hash != request_hash.as_slice() {
                return Err(StoreError::Idempotency);
            }
            return packet_result(&tx, write.stamp.packet_id, incident_revision);
        }
        let incident = get_incident(&tx, write.incident_id)?;
        if write.observation_time.is_some() {
            crate::db::verify_camera_scope(&tx, write.stamp.scope)?;
        }
        if incident.revision != write.expected_revision {
            return Err(StoreError::Revision {
                current: incident.revision,
            });
        }
        if write.context.as_ref().is_some_and(|context| {
            !matches!(
                (incident.category, context.local_facts),
                (sw_domain::Category::Medical, LocalFacts::Medical { .. })
                    | (sw_domain::Category::Fire, LocalFacts::Fire { .. })
            )
        }) {
            return Err(StoreError::Invalid);
        }
        let observed = write.observation_time.map(|time| time.observed_host_utc);
        let mut transition = if write.observation_time.is_some() {
            incident.attach_observed_evidence(write.stamp, EvidenceState::Persisted, now)?
        } else {
            incident.attach_evidence(write.stamp, EvidenceState::Persisted, now)?
        };
        let incident_revision = transition.incident.revision;
        tx.execute(
            "INSERT INTO evidence_packets(id, incident_id, version, incident_revision, captured_at, expires_at, request_hash, context, observed_host_utc, semantic_eligible)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 0)",
            params![packet, id(&incident.id), sql_counter(write.stamp.version)?, sql_counter(incident_revision)?, observed.unwrap_or(now),
                write.stamp.expires_at, request_hash.as_slice(), write.context.as_ref().map(serde_json::to_string).transpose()?, observed],
        )?;

        if self.blob_bytes.is_none() {
            self.blob_bytes = Some(physical_bytes(&self.blobs)?);
        }
        let mut result = EvidenceResult {
            packet_id: write.stamp.packet_id,
            incident_revision,
            available: 0,
            failures: Vec::new(),
        };
        for (ordinal, (image, digest)) in write.images.iter().zip(image_hashes).enumerate() {
            let info = BlobInfo {
                digest,
                byte_length: image.jpeg.len() as u64,
            };
            let path = blob_path(&self.blobs, &info.digest)?;
            let failure = if path.is_file() {
                verify_blob(&path, &info).err().map(|_| EvidenceFailure::Io)
            } else {
                match self.blob_bytes {
                    Some(used) if info.byte_length <= self.quota.saturating_sub(used) => {
                        if write_blob(&self.blobs, &path, &image.jpeg).is_ok() {
                            // Charge writes even if the metadata transaction rolls back.
                            self.blob_bytes = Some(used.saturating_add(info.byte_length));
                            None
                        } else {
                            // A failed directory flush can leave a renamed blob.
                            self.blob_bytes = None;
                            Some(EvidenceFailure::Io)
                        }
                    }
                    Some(_) => Some(EvidenceFailure::Quota),
                    None => Some(EvidenceFailure::Io),
                }
            };
            let ordinal = ordinal as u8;
            let kind = match image.kind {
                EvidenceImageKind::Overview => "overview",
                EvidenceImageKind::Crop => "crop",
            };
            if let Some(failure) = failure {
                let reason = if failure == EvidenceFailure::Quota {
                    "quota"
                } else {
                    "io"
                };
                tx.execute("INSERT INTO evidence_images(packet_id, ordinal, source_us, failure, kind) VALUES (?, ?, ?, ?, ?)", params![packet, ordinal, image.source_us, reason, kind])?;
                result.failures.push((ordinal, failure));
            } else {
                tx.execute("INSERT INTO artifacts VALUES (?, ?, 1) ON CONFLICT(digest) DO UPDATE SET available = 1", params![info.digest, sql_counter(info.byte_length)?])?;
                tx.execute("INSERT INTO evidence_images(packet_id, ordinal, digest, source_us, kind) VALUES (?, ?, ?, ?, ?)", params![packet, ordinal, info.digest, image.source_us, kind])?;
                result.available += 1;
            }
        }
        let outcome = EvidenceOutcome::new(result);
        transition.incident.evidence_state = match outcome {
            EvidenceOutcome::Persisted(_) => EvidenceState::Persisted,
            EvidenceOutcome::Partial(_) => EvidenceState::Partial,
            EvidenceOutcome::Failed(_) => EvidenceState::Unavailable,
        };
        update_incident(&tx, &transition)?;
        // A storage stall can cross the deadline while preparing the transaction.
        let committed_at = if write.observation_time.is_some() {
            crate::utc_ms()
        } else {
            now
        };
        let eligible = write.observation_time.is_some_and(|time| {
            time.evidence_fresh(now, monotonic) && time.evidence_fresh(committed_at, Instant::now())
        });
        tx.execute(
            "UPDATE evidence_packets SET persisted_at = ?, semantic_eligible = ? WHERE id = ?",
            params![committed_at, eligible, packet],
        )?;
        tx.commit()?;
        if eligible
            && matches!(outcome, EvidenceOutcome::Persisted(_))
            && let Some(time) = write.observation_time
        {
            let now = Instant::now();
            self.evidence_monotonic
                .retain(|_, registered| registered.evidence_fresh(committed_at, now));
            const MAX_FRESH_EVIDENCE: usize = 2_048;
            if self.evidence_monotonic.len() >= MAX_FRESH_EVIDENCE
                && let Some(oldest) = self
                    .evidence_monotonic
                    .iter()
                    .min_by_key(|(_, time)| time.observed_at)
                    .map(|(&packet, _)| packet)
            {
                self.evidence_monotonic.remove(&oldest);
            }
            self.evidence_monotonic.insert(write.stamp.packet_id, time);
        }
        self.notify.send_replace(());
        Ok(outcome)
    }

    pub fn evidence_unavailable(
        &mut self,
        incident: Uuid,
        now: UnixMillis,
    ) -> Result<(), StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let incident = get_incident(&tx, incident)?;
        if incident.evidence_state != EvidenceState::Pending {
            return Ok(());
        }
        update_incident(&tx, &incident.evidence_unavailable(now)?)?;
        tx.commit()?;
        self.notify.send_replace(());
        Ok(())
    }

    pub fn evidence_packet(
        &self,
        incident: Uuid,
        packet: Uuid,
    ) -> Result<EvidencePacket, StoreError> {
        let item = get_incident(&self.conn, incident)?;
        let (version, expires, context, observed, persisted, eligible) = self.conn.query_row(
            "SELECT version, expires_at, context, observed_host_utc, persisted_at, semantic_eligible FROM evidence_packets WHERE incident_id = ? AND id = ?",
            params![id(&incident), id(&packet)], |r| Ok((row_counter(r, 0)?, r.get::<_, i64>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<i64>>(3)?, r.get::<_, Option<i64>>(4)?, r.get::<_, bool>(5)?)),
        ).optional()?.ok_or(StoreError::NotFound)?;
        let mut statement = self.conn.prepare(
            "SELECT e.ordinal, e.source_us, e.digest, e.kind, e.retired = 0 AND coalesce(a.available, 0)
             FROM evidence_images e LEFT JOIN artifacts a ON a.digest = e.digest
             WHERE e.packet_id = ? ORDER BY e.ordinal",
        )?;
        let images = statement
            .query_map([id(&packet)], |r| {
                let ordinal = r.get::<_, u8>(0)?;
                let available = r.get::<_, bool>(4)?;
                let kind = match r.get::<_, String>(3)?.as_str() {
                    "overview" => EvidenceImageKind::Overview,
                    "crop" => EvidenceImageKind::Crop,
                    _ => return Err(rusqlite::Error::InvalidQuery),
                };
                Ok(EvidenceImageMetadata {
                    image_id: format!("f{ordinal}"),
                    ordinal,
                    source_s: r.get::<_, i64>(1)? as f64 / 1_000_000.0,
                    kind,
                    sha256: r.get(2)?,
                    mime: "image/jpeg".into(),
                    availability: if available {
                        EvidenceImageAvailability::Available
                    } else {
                        EvidenceImageAvailability::Unavailable
                    },
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(EvidencePacket {
            schema_version: "1.0".into(),
            packet_id: packet,
            incident_id: incident,
            camera_id: item.scope.camera_id,
            source_epoch: Counter(item.scope.epoch),
            evidence_version: Counter(version),
            scene_revision: Counter(item.scope.scene_revision),
            category: match item.category {
                sw_domain::Category::Medical => Category::Medical,
                sw_domain::Category::Fire => Category::Fire,
                sw_domain::Category::Overview => Category::Overview,
            },
            context: context.as_deref().map(serde_json::from_str).transpose()?,
            images,
            expires_at: UtcTimestamp::from_unix_millis(expires).map_err(|_| StoreError::Schema)?,
            observed_host_utc: observed
                .map(UtcTimestamp::from_unix_millis)
                .transpose()
                .map_err(|_| StoreError::Schema)?,
            persisted_at: persisted
                .map(UtcTimestamp::from_unix_millis)
                .transpose()
                .map_err(|_| StoreError::Schema)?,
            fresh_when_persisted: observed.map(|_| eligible),
        })
    }

    pub fn read_evidence(
        &mut self,
        incident: Uuid,
        packet: Uuid,
        ordinal: u8,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let info = self.conn.query_row(
            "SELECT a.digest, a.byte_length FROM evidence_packets p JOIN evidence_images i ON p.id = i.packet_id
             JOIN artifacts a ON i.digest = a.digest WHERE p.incident_id = ? AND p.id = ? AND i.ordinal = ? AND i.retired = 0 AND a.available = 1",
            params![id(&incident), id(&packet), ordinal], |row| Ok(BlobInfo { digest: row.get(0)?, byte_length: row_counter(row, 1)? }),
        ).optional()?;
        let Some(info) = info else { return Ok(None) };
        let path = blob_path(&self.blobs, &info.digest)?;
        let mut bytes = Vec::with_capacity(info.byte_length.min(MAX_IMAGE as u64) as usize);
        File::open(path)?
            .take(MAX_IMAGE as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != info.byte_length
            || bytes.len() > MAX_IMAGE
            || hex_digest(&bytes) != info.digest
        {
            return Err(StoreError::Invalid);
        }
        Ok(Some(bytes))
    }

    pub fn retire_evidence(
        &mut self,
        cutoff: UnixMillis,
        now: UnixMillis,
    ) -> Result<(), StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS due_packets(id BLOB PRIMARY KEY, incident_id BLOB NOT NULL) WITHOUT ROWID;
                          DELETE FROM due_packets;")?;
        tx.execute(
            "INSERT INTO due_packets
             SELECT p.id, p.incident_id FROM evidence_packets p
             WHERE p.captured_at <= ? AND EXISTS (SELECT 1 FROM evidence_images i WHERE i.packet_id = p.id AND i.retired = 0 AND i.digest IS NOT NULL)
             ORDER BY p.captured_at, p.id LIMIT 128",
            [cutoff],
        )?;
        let removed = {
            let mut stmt = tx.prepare(
                "SELECT DISTINCT i.digest FROM evidence_images i
                 JOIN due_packets p ON i.packet_id = p.id WHERE i.retired = 0 AND i.digest IS NOT NULL",
            )?;
            stmt.query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        tx.execute(
            "UPDATE evidence_images SET retired = 1 WHERE packet_id IN (SELECT id FROM due_packets)",
            [],
        )?;
        for digest in removed {
            if tx.execute(
                "UPDATE artifacts SET available = 0 WHERE digest = ?
                 AND NOT EXISTS (SELECT 1 FROM evidence_images WHERE digest = artifacts.digest AND retired = 0)",
                [&digest],
            )? != 0 {
                tx.execute(
                    "INSERT OR IGNORE INTO retired_blob_cleanup VALUES (?)",
                    [&digest],
                )?;
            }
        }
        let states = {
            let mut stmt = tx.prepare(
                "SELECT i.state FROM incidents i JOIN due_packets p ON p.incident_id = i.id
                 AND lower(hex(p.id)) = replace(json_extract(i.state, '$.evidence.packet_id'), '-', '')",
            )?;
            stmt.query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let changed = !states.is_empty();
        for state in states {
            let mut incident: Incident = serde_json::from_str(&state)?;
            if incident.evidence_state == EvidenceState::Unavailable {
                continue;
            }
            incident.evidence_state = EvidenceState::Unavailable;
            incident.revision = incident
                .revision
                .checked_add(1)
                .ok_or(StoreError::Invalid)?;
            incident.updated_at = incident.updated_at.max(now);
            update_incident(
                &tx,
                &Transition {
                    incident,
                    notification: None,
                    silence: None,
                },
            )?;
        }
        tx.commit()?;
        if changed {
            self.notify.send_replace(());
        }
        // Recount on the next write after scheduled maintenance, including any
        // external loss or failed cleanup observed since the last count.
        self.blob_bytes = None;
        let cleanup = {
            let mut stmt = self
                .conn
                .prepare("SELECT digest FROM retired_blob_cleanup ORDER BY digest LIMIT 2048")?;
            stmt.query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        if !cleanup.is_empty() {
            for digest in &cleanup {
                if has_active_reference(&self.conn, digest)? {
                    continue;
                }
                match fs::remove_file(blob_path(&self.blobs, digest)?) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            sync_directory(&self.blobs)?;
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            for digest in cleanup {
                tx.execute(
                    "DELETE FROM retired_blob_cleanup WHERE digest = ?",
                    [digest],
                )?;
            }
            tx.commit()?;
        }
        Ok(())
    }

    pub fn reconcile_evidence(&mut self) -> Result<(), StoreError> {
        let known = all_blobs(&self.conn)?;
        for entry in fs::read_dir(&self.blobs)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let orphan = name
                .strip_suffix(".jpg")
                .filter(|digest| valid_digest(digest))
                .is_some_and(|digest| {
                    known
                        .binary_search_by(|blob| blob.digest.as_str().cmp(digest))
                        .is_err()
                });
            if orphan || name.starts_with(".pending-") {
                fs::remove_file(entry.path())?;
            }
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for blob in known {
            let path = blob_path(&self.blobs, &blob.digest)?;
            let active = has_active_reference(&tx, &blob.digest)?;
            let available = active && verify_blob(&path, &blob).is_ok();
            if !active && path.is_file() {
                fs::remove_file(&path)?;
            }
            tx.execute(
                "UPDATE artifacts SET available = ? WHERE digest = ? AND available != ?",
                params![available, blob.digest, available],
            )?;
        }
        let states = {
            let mut statement = tx.prepare(
                "SELECT incidents.state, count(*), sum(i.retired = 0 AND coalesce(artifacts.available, 0)) FROM incidents
                 JOIN evidence_packets p ON p.incident_id = incidents.id
                 JOIN evidence_images i ON p.id = i.packet_id LEFT JOIN artifacts ON i.digest = artifacts.digest
                 WHERE p.version = (SELECT max(version) FROM evidence_packets WHERE incident_id = incidents.id)
                 GROUP BY incidents.id",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row_counter(row, 1)?,
                        row_counter(row, 2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (state, expected, available) in states {
            let mut incident: Incident = serde_json::from_str(&state)?;
            let evidence = if available == 0 {
                EvidenceState::Unavailable
            } else if available == expected {
                EvidenceState::Persisted
            } else {
                EvidenceState::Partial
            };
            if incident.evidence_state == evidence {
                continue;
            }
            incident.evidence_state = evidence;
            incident.revision = incident
                .revision
                .checked_add(1)
                .ok_or(StoreError::Invalid)?;
            incident.updated_at = incident.updated_at.max(now_millis()?);
            update_incident(
                &tx,
                &Transition {
                    incident,
                    notification: None,
                    silence: None,
                },
            )?;
        }
        // Startup reconciliation also covers a crash after the incident and
        // outbox commit but before its packet was attached.
        let pending = {
            let mut statement = tx.prepare(
                "SELECT state FROM incidents WHERE camera_hazard = 1 AND json_extract(state, '$.evidence_state') = 'pending'",
            )?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let now = now_millis()?;
        for state in pending {
            let incident: Incident = serde_json::from_str(&state)?;
            update_incident(&tx, &incident.evidence_unavailable(now)?)?;
        }
        tx.commit()?;
        self.blob_bytes = Some(physical_bytes(&self.blobs)?);
        Ok(())
    }
}

fn has_active_reference(conn: &Connection, digest: &str) -> Result<bool, StoreError> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM evidence_images WHERE digest = ? AND retired = 0)",
        [digest],
        |row| row.get(0),
    )?)
}

fn packet_result(
    conn: &Connection,
    packet_id: Uuid,
    incident_revision: u64,
) -> Result<EvidenceOutcome, StoreError> {
    let mut statement = conn.prepare(
        "SELECT i.ordinal, i.failure, i.retired = 0 AND coalesce(a.available, 0) FROM evidence_images i LEFT JOIN artifacts a ON i.digest = a.digest WHERE i.packet_id = ? ORDER BY i.ordinal",
    )?;
    let rows = statement.query_map([id(&packet_id)], |row| {
        Ok((
            row.get::<_, u8>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, bool>(2)?,
        ))
    })?;
    let mut result = EvidenceResult {
        packet_id,
        incident_revision,
        available: 0,
        failures: Vec::new(),
    };
    for row in rows {
        let (ordinal, failure, available) = row?;
        if available {
            result.available += 1;
        } else {
            let failure = match failure.as_deref() {
                Some("quota") => EvidenceFailure::Quota,
                Some("io") => EvidenceFailure::Io,
                None => EvidenceFailure::Missing,
                _ => return Err(StoreError::Invalid),
            };
            result.failures.push((ordinal, failure));
        }
    }
    Ok(EvidenceOutcome::new(result))
}

fn physical_bytes(root: &Path) -> Result<u64, StoreError> {
    let mut size = 0u64;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            size = size.saturating_add(entry.metadata()?.len());
        }
    }
    Ok(size)
}

pub(crate) fn all_blobs(conn: &Connection) -> Result<Vec<BlobInfo>, StoreError> {
    let mut statement =
        conn.prepare("SELECT digest, byte_length FROM artifacts ORDER BY digest")?;
    Ok(statement
        .query_map([], |row| {
            Ok(BlobInfo {
                digest: row.get(0)?,
                byte_length: row_counter(row, 1)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?)
}

pub(crate) fn available_blobs(conn: &Connection) -> Result<Vec<BlobInfo>, StoreError> {
    let mut statement = conn
        .prepare("SELECT digest, byte_length FROM artifacts WHERE available = 1 ORDER BY digest")?;
    Ok(statement
        .query_map([], |row| {
            Ok(BlobInfo {
                digest: row.get(0)?,
                byte_length: row_counter(row, 1)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?)
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
}

pub(crate) fn blob_path(root: &Path, digest: &str) -> Result<PathBuf, StoreError> {
    if !valid_digest(digest) {
        return Err(StoreError::Invalid);
    }
    Ok(root.join(format!("{digest}.jpg")))
}

pub(crate) fn verify_blob(path: &Path, info: &BlobInfo) -> Result<(), StoreError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file()
        || metadata.len() != info.byte_length
        || metadata.len() > MAX_IMAGE as u64
    {
        return Err(StoreError::Invalid);
    }
    if file_digest(path)? != info.digest {
        return Err(StoreError::Invalid);
    }
    Ok(())
}

pub(crate) fn file_digest(path: &Path) -> Result<String, StoreError> {
    let mut hash = Sha256::new();
    io::copy(&mut File::open(path)?, &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}

pub(crate) fn hex_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn write_blob(root: &Path, path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let mut staging = tempfile::Builder::new()
        .prefix(".pending-")
        .tempfile_in(root)?;
    staging.write_all(bytes)?;
    staging.as_file().sync_all()?;
    staging
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    sync_directory(root)?;
    Ok(())
}

pub(crate) fn sync_directory(path: &Path) -> Result<(), StoreError> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub(crate) fn now_millis() -> Result<UnixMillis, StoreError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StoreError::Invalid)?;
    i64::try_from(duration.as_millis()).map_err(|_| StoreError::Invalid)
}
