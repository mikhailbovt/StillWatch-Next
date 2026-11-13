use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::Duration,
};

use rusqlite::{Connection, OpenFlags, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sw_domain::{
    Acknowledgement, Assessment, Attention, Category, EvidenceStamp, EvidenceState, Incident,
    NewIncident, Resolution, SourceScope, Transition,
};
use sw_protocol::{CameraConfig, UtcTimestamp};
use uuid::Uuid;

use crate::{
    StoreConfig, StoreError,
    backup::{fresh_destination, validate_database},
    db::{
        DATABASE_FILE, SCHEMA_VERSION, append_change, get_incident, id, migrate, private_dir,
        read_identity, row_counter, sql_counter,
    },
    evidence::{
        MAX_IMAGE, blob_path, file_digest, hex_digest, now_millis, sync_directory, write_blob,
    },
};

const MAX_ROW_BYTES: i64 = 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyImport {
    pub camera: CameraConfig,
    /// Legacy masks used normalized coordinates relative to the old field of view.
    #[serde(default)]
    pub same_geometry: bool,
    pub evidence_quota_bytes: Option<u64>,
}

#[derive(Serialize)]
pub struct LegacyManifest {
    pub source_schema: u8,
    pub destination_schema: i64,
    pub instance_id: Uuid,
    pub camera_id: Uuid,
    pub camera_enabled: bool,
    pub privacy_masks_need_review: bool,
    pub incidents: u64,
    pub needs_reassessment: u64,
    pub scene_overviews_archived: u64,
    pub assessments_archived: u64,
    pub feedback_archived: u64,
    pub packets_archived: u64,
    pub historical_images_listed: u64,
    pub historical_images_available: u64,
    pub snapshot_sha256: String,
    pub destination_database_sha256: String,
}

#[derive(Deserialize)]
struct LegacyIncident {
    incident_id: String,
    camera_id: String,
    category: LegacyCategory,
    reason: String,
    first_ts: f64,
    last_ts: f64,
    epoch: u64,
    lifecycle: LegacyLifecycle,
    created_at: f64,
    updated_at: f64,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LegacyCategory {
    Medical,
    Fire,
    Overview,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum LegacyLifecycle {
    Open,
    Acknowledged,
    Closed,
}

/// Import an offline schema-1 snapshot into a fresh database. The source
/// connection stays read-only and the output is published with a directory rename.
pub fn import_legacy(
    source: &Path,
    destination: &Path,
    mut options: LegacyImport,
    dry_run: bool,
) -> Result<LegacyManifest, StoreError> {
    let source = source.canonicalize()?;
    let parent = fresh_destination(destination)?;
    if parent.canonicalize()?.starts_with(&source) {
        return Err(StoreError::Invalid);
    }
    let db_file = source.join(DATABASE_FILE);
    if !fs::symlink_metadata(&db_file)?.file_type().is_file() {
        return Err(StoreError::Invalid);
    }
    let mut input = Connection::open_with_flags(&db_file, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    input.busy_timeout(Duration::from_secs(3))?;
    input.execute_batch("PRAGMA query_only = ON; PRAGMA trusted_schema = OFF;")?;
    let snapshot = input.transaction()?;
    check_legacy_schema(&snapshot)?;

    options.camera.enabled = false;
    options.camera.cloud_consent = false;
    let privacy_masks_need_review = !options.same_geometry;
    if !options.camera.valid() {
        return Err(StoreError::Invalid);
    }
    let stage = tempfile::Builder::new()
        .prefix(".stillwatch-migrate-")
        .tempdir_in(parent)?;
    let destination_db = stage.path().join(DATABASE_FILE);
    let blob_dir = stage.path().join("blobs");
    private_dir(&blob_dir)?;
    let mut output = Connection::open(&destination_db)?;
    output.execute_batch(
        "PRAGMA synchronous = FULL; PRAGMA foreign_keys = ON; PRAGMA trusted_schema = OFF;",
    )?;
    migrate(&mut output)?;
    let identity = read_identity(&output)?;
    let camera_id = legacy_camera_id(&snapshot)?;
    let mut manifest = LegacyManifest {
        source_schema: 1,
        destination_schema: SCHEMA_VERSION,
        instance_id: identity.instance_id,
        camera_id,
        camera_enabled: false,
        privacy_masks_need_review,
        incidents: 0,
        needs_reassessment: 0,
        scene_overviews_archived: 0,
        assessments_archived: 0,
        feedback_archived: 0,
        packets_archived: 0,
        historical_images_listed: 0,
        historical_images_available: 0,
        snapshot_sha256: String::new(),
        destination_database_sha256: String::new(),
    };
    let mut fingerprint = Sha256::new();
    let tx = output.transaction()?;
    let config = serde_json::to_string(&options.camera)?;
    tx.execute(
        "INSERT INTO cameras(id, revision, scene_revision, source_epoch, enabled, deleted, bytes_reserved, config)
         VALUES (?, 1, 1, 0, 0, 0, 0, ?)",
        params![id(&camera_id), config],
    )?;
    tx.execute(
        "INSERT INTO camera_config_versions VALUES (?, 1, ?, ?, ?, 0)",
        params![id(&camera_id), id(&Uuid::nil()), now_millis()?, config],
    )?;
    import_incidents(&snapshot, &tx, &mut manifest, &mut fingerprint)?;
    import_history(&snapshot, &tx, &mut manifest, &mut fingerprint)?;
    import_packets(
        &snapshot,
        &tx,
        &mut manifest,
        &mut fingerprint,
        LegacyEvidence {
            source: source.join("incidents"),
            destination: &blob_dir,
            remaining: options
                .evidence_quota_bytes
                .unwrap_or_else(|| StoreConfig::default().evidence_quota_bytes),
        },
    )?;
    {
        let mut changes = tx.prepare("SELECT state FROM incidents ORDER BY updated_at, id")?;
        let mut rows = changes.query([])?;
        while let Some(row) = rows.next()? {
            let state: String = row.get(0)?;
            let transition = Transition {
                incident: serde_json::from_str(&state)?,
                notification: None,
                silence: None,
            };
            append_change(&tx, &transition, true, &state)?;
        }
    }
    tx.commit()?;
    validate_database(&output, SCHEMA_VERSION)?;
    output.close().map_err(|(_, error)| error)?;
    OpenOptions::new()
        .write(true)
        .open(&destination_db)?
        .sync_all()?;
    manifest.snapshot_sha256 = format!("{:x}", fingerprint.finalize());
    manifest.destination_database_sha256 = file_digest(&destination_db)?;

    if !dry_run {
        let mut file = File::create_new(stage.path().join("migration.json"))?;
        serde_json::to_writer_pretty(&mut file, &manifest)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        sync_directory(stage.path())?;
        fresh_destination(destination)?;
        fs::rename(stage.path(), destination)?;
        sync_directory(parent)?;
    }
    Ok(manifest)
}

fn check_legacy_schema(source: &Connection) -> Result<(), StoreError> {
    let version: i64 = source.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let application: i64 = source.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let tables: i64 = source.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type = 'table'
         AND name IN ('incidents','assessments','packets','feedback','calls')",
        [],
        |row| row.get(0),
    )?;
    if version != 1 || application != 0 || tables != 5 {
        return Err(StoreError::Schema);
    }
    Ok(())
}

fn legacy_camera_id(source: &Connection) -> Result<Uuid, StoreError> {
    let mut query = source.prepare(
        "SELECT DISTINCT CASE WHEN length(body) <= ? THEN json_extract(body, '$.camera_id') END FROM incidents LIMIT 2",
    )?;
    let mut rows = query.query([MAX_ROW_BYTES])?;
    let Some(row) = rows.next()? else {
        return Ok(Uuid::new_v4());
    };
    let camera: String = row.get(0)?;
    if rows.next()?.is_some() {
        return Err(StoreError::Invalid);
    }
    Ok(Uuid::parse_str(&camera).unwrap_or_else(|_| Uuid::new_v4()))
}

fn import_incidents(
    source: &Connection,
    tx: &Transaction<'_>,
    manifest: &mut LegacyManifest,
    fingerprint: &mut Sha256,
) -> Result<(), StoreError> {
    let mut query = source.prepare(
        "SELECT id, CASE WHEN length(body) <= ? THEN body END FROM incidents ORDER BY updated, id",
    )?;
    let mut rows = query.query([MAX_ROW_BYTES])?;
    while let Some(row) = rows.next()? {
        let original_id: String = row.get(0)?;
        let body: String = row.get(1)?;
        let old: LegacyIncident = serde_json::from_str(&body)?;
        if old.incident_id != original_id {
            return Err(StoreError::Invalid);
        }
        let incident_id = parse_id(&original_id)?;
        let archived_overview = matches!(old.category, LegacyCategory::Overview);
        let resolved = archived_overview || matches!(old.lifecycle, LegacyLifecycle::Closed);
        let category = match old.category {
            LegacyCategory::Fire => Category::Fire,
            LegacyCategory::Medical => Category::Medical,
            LegacyCategory::Overview => Category::Overview,
        };
        let created_at = timestamp(old.created_at)?;
        let updated_at = timestamp(old.updated_at)?.max(created_at);
        let reason = if archived_overview {
            "Legacy scene overview".into()
        } else {
            old.reason
        };
        let mut incident = Incident::create(
            NewIncident {
                id: incident_id,
                scope: SourceScope {
                    camera_id: manifest.camera_id,
                    epoch: old.epoch,
                    scene_revision: 1,
                },
                category,
                reason,
                first_source_us: scaled(old.first_ts, 1_000_000.0)?,
                last_source_us: scaled(old.last_ts, 1_000_000.0)?,
                live: false,
                assessment: Assessment::Unknown,
            },
            created_at,
        )?
        .incident;
        incident.updated_at = updated_at;
        incident.evidence_state = EvidenceState::Unavailable;
        incident.needs_reassessment = !resolved;
        if resolved {
            incident.resolution = Some(Resolution {
                actor_id: None,
                at: updated_at,
                reason: if archived_overview {
                    "Archived legacy scene overview"
                } else {
                    "Closed in legacy StillWatch"
                }
                .into(),
            });
        } else {
            incident.attention = Attention::Review;
            incident.attention_cycle = 1;
            if matches!(old.lifecycle, LegacyLifecycle::Acknowledged) {
                incident.acknowledgement = Some(Acknowledgement {
                    actor_id: Uuid::nil(),
                    at: updated_at,
                });
            }
        }
        let hash = Sha256::digest(body.as_bytes());
        let state = serde_json::to_string(&incident)?;
        tx.execute(
            "INSERT INTO incidents(id, camera_id, revision, created_at, updated_at, resolved, attention, creation_hash, state)
             VALUES (?, ?, 1, ?, ?, ?, ?, ?, ?)",
            params![id(&incident.id), id(&manifest.camera_id), created_at, updated_at, resolved, i64::from(!resolved), hash.as_slice(), state],
        )?;
        tx.execute(
            "INSERT INTO legacy_incidents VALUES (?, ?, ?)",
            params![id(&incident.id), old.camera_id, body],
        )?;
        manifest.incidents += 1;
        manifest.needs_reassessment += u64::from(!resolved);
        manifest.scene_overviews_archived += u64::from(archived_overview);
        digest_part(fingerprint, b"incident", body.as_bytes());
    }
    Ok(())
}

fn import_history(
    source: &Connection,
    tx: &Connection,
    manifest: &mut LegacyManifest,
    fingerprint: &mut Sha256,
) -> Result<(), StoreError> {
    let mut query = source.prepare(
        "SELECT id, incident_id, created, CASE WHEN length(packet) <= ?1 THEN packet END,
                CASE WHEN length(result) <= ?1 THEN result END FROM assessments ORDER BY id",
    )?;
    let mut rows = query.query([MAX_ROW_BYTES])?;
    while let Some(row) = rows.next()? {
        let key: i64 = row.get(0)?;
        let incident = parse_id(&row.get::<_, String>(1)?)?;
        let created = timestamp(row.get(2)?)?;
        let packet: String = row.get(3)?;
        let result: String = row.get(4)?;
        let provider: serde_json::Value = serde_json::from_str(&result)?;
        let provider = provider.get("provider").and_then(|value| value.as_str());
        tx.execute(
            "INSERT INTO legacy_assessments VALUES (?, ?, ?, ?, ?, ?)",
            params![key, id(&incident), created, provider, packet, result],
        )?;
        digest_part(
            fingerprint,
            b"assessment",
            &serde_json::to_vec(&(key, incident, created, packet, result))?,
        );
        manifest.assessments_archived += 1;
    }
    let mut query = source.prepare(
        "SELECT id, incident_id, created, CASE WHEN length(operator) <= ?1 THEN operator END,
                CASE WHEN length(action) <= ?1 THEN action END FROM feedback ORDER BY id",
    )?;
    let mut rows = query.query([MAX_ROW_BYTES])?;
    while let Some(row) = rows.next()? {
        let key: i64 = row.get(0)?;
        let incident = parse_id(&row.get::<_, String>(1)?)?;
        let created = timestamp(row.get(2)?)?;
        let operator: String = row.get(3)?;
        let action: String = row.get(4)?;
        tx.execute(
            "INSERT INTO legacy_feedback VALUES (?, ?, ?, ?, ?)",
            params![key, id(&incident), created, operator, action],
        )?;
        digest_part(
            fingerprint,
            b"feedback",
            &serde_json::to_vec(&(key, incident, created, operator, action))?,
        );
        manifest.feedback_archived += 1;
    }
    Ok(())
}

#[derive(Deserialize)]
struct LegacyPacket {
    version: u64,
    images: Vec<LegacyImage>,
}

#[derive(Deserialize)]
struct LegacyImage {
    capture_ts: f64,
    kind: String,
}

struct LegacyEvidence<'a> {
    source: PathBuf,
    destination: &'a Path,
    remaining: u64,
}

impl LegacyEvidence<'_> {
    fn import(&mut self, tx: &Connection, path: &str) -> Result<Option<String>, StoreError> {
        let relative = Path::new(path);
        if relative.as_os_str().is_empty()
            || !relative
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
            || !relative
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("jpg"))
        {
            return Ok(None);
        }
        let source = match self.source.canonicalize() {
            Ok(directory) if directory == self.source => directory,
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let original = source.join(relative);
        let path = match original.canonicalize() {
            Ok(path) if path.starts_with(&source) => path,
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let metadata = fs::symlink_metadata(&original)?;
        if !metadata.file_type().is_file() || metadata.len() > MAX_IMAGE as u64 {
            return Ok(None);
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        File::open(path)?
            .take(MAX_IMAGE as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_IMAGE
            || !bytes.starts_with(&[0xff, 0xd8, 0xff])
            || !bytes.ends_with(&[0xff, 0xd9])
        {
            return Ok(None);
        }
        let digest = hex_digest(&bytes);
        let destination = blob_path(self.destination, &digest)?;
        if !destination.exists() {
            if bytes.len() as u64 > self.remaining {
                return Ok(None);
            }
            write_blob(self.destination, &destination, &bytes)?;
            tx.execute(
                "INSERT INTO artifacts VALUES (?, ?, 1)",
                params![digest, bytes.len() as i64],
            )?;
            self.remaining -= bytes.len() as u64;
        }
        Ok(Some(digest))
    }
}

fn import_packets(
    source: &Connection,
    tx: &Connection,
    manifest: &mut LegacyManifest,
    fingerprint: &mut Sha256,
    mut evidence: LegacyEvidence<'_>,
) -> Result<(), StoreError> {
    let mut query = source.prepare(
        "SELECT id, incident_id, created, CASE WHEN length(metadata) <= ?1 THEN metadata END,
                CASE WHEN length(files) <= ?1 THEN files END FROM packets ORDER BY created, id",
    )?;
    let mut rows = query.query([MAX_ROW_BYTES])?;
    while let Some(row) = rows.next()? {
        let packet = parse_id(&row.get::<_, String>(0)?)?;
        let incident = parse_id(&row.get::<_, String>(1)?)?;
        let created = timestamp(row.get(2)?)?;
        let metadata: String = row.get(3)?;
        let files: String = row.get(4)?;
        let paths: Vec<String> = serde_json::from_str(&files)?;
        let data: LegacyPacket = serde_json::from_str(&metadata)?;
        if data.images.len() > 16 || paths.len() > data.images.len() {
            return Err(StoreError::Invalid);
        }
        manifest.historical_images_listed += data.images.len() as u64;
        let hash = Sha256::digest(metadata.as_bytes());
        tx.execute(
            "INSERT INTO legacy_packets VALUES (?, ?, ?, ?, ?)",
            params![
                id(&packet),
                id(&incident),
                created,
                metadata,
                hash.as_slice()
            ],
        )?;
        let mut incident_state = get_incident(tx, incident)?;
        let used: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM evidence_packets WHERE incident_id = ? AND version = ?)",
            params![id(&incident), sql_counter(data.version)?],
            |row| row.get(0),
        )?;
        let version = if data.version == 0 || used {
            tx.query_row(
                "SELECT coalesce(max(version), 0) + 1 FROM evidence_packets WHERE incident_id = ?",
                [id(&incident)],
                |row| row_counter(row, 0),
            )?
        } else {
            data.version
        };
        tx.execute(
            "INSERT INTO evidence_packets(id, incident_id, version, incident_revision, captured_at, expires_at, request_hash)
             VALUES (?, ?, ?, 1, ?, ?, ?)",
            params![id(&packet), id(&incident), sql_counter(version)?, created, created, hash.as_slice()],
        )?;
        let mut available = 0;
        for (ordinal, image) in data.images.iter().enumerate() {
            let kind = match image.kind.as_str() {
                "overview" => "overview",
                "crop" => "crop",
                _ => return Err(StoreError::Invalid),
            };
            let digest = match paths.get(ordinal) {
                Some(path) => evidence.import(tx, path)?,
                None => None,
            };
            let failure = digest.is_none().then_some("io");
            tx.execute(
                "INSERT INTO evidence_images(packet_id, ordinal, digest, source_us, kind, failure)
                 VALUES (?, ?, ?, ?, ?, ?)",
                params![
                    id(&packet),
                    ordinal as u8,
                    digest,
                    scaled(image.capture_ts, 1_000_000.0)?,
                    kind,
                    failure
                ],
            )?;
            available += usize::from(digest.is_some());
            if let Some(value) = digest {
                digest_part(fingerprint, b"jpeg", value.as_bytes());
            }
        }
        manifest.historical_images_available += available as u64;
        if version > incident_state.evidence_version() {
            incident_state.evidence = Some(EvidenceStamp {
                packet_id: packet,
                version,
                scope: incident_state.scope,
                expires_at: created,
            });
            incident_state.evidence_state = match available {
                0 => EvidenceState::Unavailable,
                n if n == data.images.len() => EvidenceState::Persisted,
                _ => EvidenceState::Partial,
            };
            tx.execute(
                "UPDATE incidents SET state = ? WHERE id = ?",
                params![serde_json::to_string(&incident_state)?, id(&incident)],
            )?;
        }
        digest_part(
            fingerprint,
            b"packet",
            &serde_json::to_vec(&(packet, incident, created, metadata, files))?,
        );
        manifest.packets_archived += 1;
    }
    Ok(())
}

fn parse_id(value: &str) -> Result<Uuid, StoreError> {
    Uuid::parse_str(value).map_err(|_| StoreError::Invalid)
}

fn timestamp(seconds: f64) -> Result<i64, StoreError> {
    let millis = scaled(seconds, 1_000.0)?;
    UtcTimestamp::from_unix_millis(millis).map_err(|_| StoreError::Invalid)?;
    Ok(millis)
}

fn scaled(value: f64, units: f64) -> Result<i64, StoreError> {
    let scaled = (value * units).round();
    if !scaled.is_finite() || scaled < i64::MIN as f64 || scaled >= i64::MAX as f64 {
        return Err(StoreError::Invalid);
    }
    Ok(scaled as i64)
}

fn digest_part(digest: &mut Sha256, kind: &[u8], value: &[u8]) {
    digest.update(kind);
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}
