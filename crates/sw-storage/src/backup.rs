use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

use rusqlite::{
    Connection, ErrorCode, OpenFlags,
    backup::{Backup, StepResult},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    Identity, StoreError,
    db::{
        APPLICATION_ID, DATABASE_FILE, Database, SCHEMA_VERSION, id, migrate, private_dir,
        read_identity,
    },
    evidence::{BlobInfo, available_blobs, blob_path, file_digest, sync_directory, verify_blob},
};

const MANIFEST: &str = "manifest.json";
const MANIFEST_LIMIT: u64 = 16 * 1024 * 1024;
const DEFAULT_BACKUP_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const DESTINATION_RESERVE_BYTES: u64 = 64 * 1024 * 1024;

/// A per-operation limit for the database, manifest and evidence bytes.
#[derive(Clone, Copy, Debug)]
pub struct BackupLimits {
    pub max_bytes: u64,
}

impl Default for BackupLimits {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_BACKUP_BYTES,
        }
    }
}

impl BackupLimits {
    fn plan(
        self,
        parent: &Path,
        database: u64,
        manifest: u64,
        blobs: &[BlobInfo],
    ) -> Result<u64, StoreError> {
        let bytes = self.ensure_size(database, manifest, blobs)?;
        if bytes.saturating_add(DESTINATION_RESERVE_BYTES) > fs4::available_space(parent)? {
            return Err(StoreError::BackupCapacity);
        }
        Ok(bytes)
    }

    fn ensure_size(
        self,
        database: u64,
        manifest: u64,
        blobs: &[BlobInfo],
    ) -> Result<u64, StoreError> {
        let bytes = blobs
            .iter()
            .try_fold(
                database
                    .checked_add(manifest)
                    .ok_or(StoreError::BackupCapacity)?,
                |sum, blob| sum.checked_add(blob.byte_length),
            )
            .ok_or(StoreError::BackupCapacity)?;
        if bytes > self.max_bytes {
            return Err(StoreError::BackupCapacity);
        }
        Ok(bytes)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: i64,
    instance_id: Uuid,
    generation: Uuid,
    database_sha256: String,
    blobs: Vec<BlobInfo>,
}

impl Database {
    pub fn backup(&mut self, destination: &Path, limits: BackupLimits) -> Result<(), StoreError> {
        let parent = fresh_destination(destination)?;
        let pages: i64 = self
            .conn
            .pragma_query_value(None, "page_count", |row| row.get(0))?;
        let page_size: i64 = self
            .conn
            .pragma_query_value(None, "page_size", |row| row.get(0))?;
        let size = u64::try_from(pages.checked_mul(page_size).ok_or(StoreError::Invalid)?)
            .map_err(|_| StoreError::Invalid)?;
        let mut manifest = Manifest {
            schema_version: SCHEMA_VERSION,
            instance_id: self.identity.instance_id,
            generation: self.identity.generation,
            database_sha256: "0".repeat(64),
            blobs: available_blobs(&self.conn)?,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)?.len() as u64;
        if manifest_bytes > MANIFEST_LIMIT {
            return Err(StoreError::Invalid);
        }
        limits.plan(parent, size, manifest_bytes, &manifest.blobs)?;
        let stage = tempfile::Builder::new()
            .prefix(".stillwatch-backup-")
            .tempdir_in(parent)?;
        let db_path = stage.path().join(DATABASE_FILE);
        let mut target = Connection::open(&db_path)?;
        copy_database(&self.conn, &mut target, pages)?;
        target.execute_batch("PRAGMA journal_mode = DELETE; PRAGMA trusted_schema = OFF;")?;
        validate_database(&target, SCHEMA_VERSION)?;
        if available_blobs(&target)? != manifest.blobs {
            return Err(StoreError::Invalid);
        }
        let actual = fs::metadata(&db_path)?.len();
        if actual > size {
            return Err(StoreError::BackupCapacity);
        }
        let target_blobs = stage.path().join("blobs");
        private_dir(&target_blobs)?;
        copy_blobs(&manifest.blobs, &self.blobs, &target_blobs)?;
        drop(target);
        OpenOptions::new().write(true).open(&db_path)?.sync_all()?;
        manifest.database_sha256 = file_digest(&db_path)?;
        let bytes = serde_json::to_vec_pretty(&manifest)?;
        if bytes.len() as u64 > MANIFEST_LIMIT {
            return Err(StoreError::Invalid);
        }
        let mut output = File::create(stage.path().join(MANIFEST))?;
        output.write_all(&bytes)?;
        output.sync_all()?;
        drop(output);
        sync_directory(stage.path())?;
        fresh_destination(destination)?;
        fs::rename(stage.path(), destination)?;
        sync_directory(parent)?;
        Ok(())
    }
}

/// Restore into a new directory. A successful restore has a fresh SSE generation.
pub fn restore_backup(backup: &Path, destination: &Path) -> Result<Identity, StoreError> {
    restore_backup_with_limits(backup, destination, BackupLimits::default())
}

pub fn restore_backup_with_limits(
    backup: &Path,
    destination: &Path,
    limits: BackupLimits,
) -> Result<Identity, StoreError> {
    let parent = fresh_destination(destination)?;
    let mut bytes = Vec::new();
    File::open(backup.join(MANIFEST))?
        .take(MANIFEST_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MANIFEST_LIMIT {
        return Err(StoreError::Invalid);
    }
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    if !(1..=SCHEMA_VERSION).contains(&manifest.schema_version) {
        return Err(StoreError::Schema);
    }

    let source_db = backup.join(DATABASE_FILE);
    let source_metadata = fs::symlink_metadata(&source_db)?;
    if !source_metadata.file_type().is_file() {
        return Err(StoreError::Invalid);
    }
    let length = source_metadata.len();
    let planned = limits.plan(parent, length, bytes.len() as u64, &manifest.blobs)?;
    let stage = tempfile::Builder::new()
        .prefix(".stillwatch-restore-")
        .tempdir_in(parent)?;
    let restored_db = stage.path().join(DATABASE_FILE);
    copy_file(&source_db, &restored_db, length)?;
    if file_digest(&restored_db)? != manifest.database_sha256 {
        return Err(StoreError::Invalid);
    }
    let mut conn = Connection::open_with_flags(&restored_db, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    conn.execute_batch(
        "PRAGMA journal_mode = DELETE; PRAGMA synchronous = FULL;
         PRAGMA trusted_schema = OFF; PRAGMA foreign_keys = ON;",
    )?;
    validate_database(&conn, manifest.schema_version)?;
    let original = read_identity(&conn)?;
    if original.instance_id != manifest.instance_id
        || original.generation != manifest.generation
        || available_blobs(&conn)? != manifest.blobs
    {
        return Err(StoreError::Invalid);
    }
    let page_size: i64 = conn.pragma_query_value(None, "page_size", |row| row.get(0))?;
    let current_limit: i64 = conn.pragma_query_value(None, "max_page_count", |row| row.get(0))?;
    let database_budget = limits.max_bytes - (planned - length);
    let allowed_pages = database_budget / page_size as u64;
    let allowed_pages = allowed_pages.min(current_limit as u64) as i64;
    conn.pragma_update(None, "max_page_count", allowed_pages)?;
    let blobs = stage.path().join("blobs");
    private_dir(&blobs)?;
    copy_blobs(&manifest.blobs, &backup.join("blobs"), &blobs)?;
    let generation = Uuid::new_v4();
    prepare_restored_database(&mut conn, generation).map_err(|error| match &error {
        StoreError::Sql(sql) if sql.sqlite_error_code() == Some(ErrorCode::DiskFull) => {
            StoreError::BackupCapacity
        }
        _ => error,
    })?;
    drop(conn);
    let restored_length = fs::metadata(&restored_db)?.len();
    if restored_length > length {
        limits.ensure_size(restored_length, bytes.len() as u64, &manifest.blobs)?;
    }
    OpenOptions::new()
        .write(true)
        .open(restored_db)?
        .sync_all()?;
    sync_directory(stage.path())?;
    fresh_destination(destination)?;
    fs::rename(stage.path(), destination)?;
    sync_directory(parent)?;
    Ok(Identity {
        generation,
        ..original
    })
}

fn prepare_restored_database(conn: &mut Connection, generation: Uuid) -> Result<(), StoreError> {
    migrate(conn)?;
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE core_identity SET stream_generation = ? WHERE singleton = 1",
        [id(&generation)],
    )?;
    tx.execute(
        "UPDATE notification_outbox SET state = 'expired' WHERE state = 'queued'",
        [],
    )?;
    tx.execute("DELETE FROM alert_silences", [])?;
    tx.execute("DELETE FROM pairing_grants", [])?;
    tx.execute("DELETE FROM ios_push_deliveries", [])?;
    tx.execute("DELETE FROM ios_push_registrations", [])?;
    tx.execute("DELETE FROM refresh_receipts", [])?;
    tx.execute("DELETE FROM device_sessions", [])?;
    tx.execute("DELETE FROM retired_refresh_credentials", [])?;
    tx.execute(
        "UPDATE trusted_devices SET revoked_at = COALESCE(revoked_at, ?)",
        [crate::evidence::now_millis()?],
    )?;
    tx.commit()?;
    Ok(())
}

fn copy_database(
    from: &Connection,
    to: &mut Connection,
    planned_pages: i64,
) -> Result<(), StoreError> {
    let backup = Backup::new(from, to)?;
    loop {
        let step = backup.step(256)?;
        if i64::from(backup.progress().pagecount) > planned_pages {
            return Err(StoreError::BackupCapacity);
        }
        match step {
            StepResult::Done => return Ok(()),
            StepResult::More => {}
            StepResult::Busy | StepResult::Locked => return Err(StoreError::Busy),
            _ => return Err(StoreError::Unavailable),
        }
    }
}

pub(crate) fn fresh_destination(path: &Path) -> Result<&Path, StoreError> {
    if !path.is_absolute() || path.file_name().is_none() || fs::symlink_metadata(path).is_ok() {
        return Err(StoreError::Invalid);
    }
    let parent = path.parent().ok_or(StoreError::Invalid)?;
    if !parent.is_dir() {
        return Err(StoreError::Invalid);
    }
    Ok(parent)
}

fn copy_blobs(blobs: &[BlobInfo], source: &Path, target: &Path) -> Result<(), StoreError> {
    for blob in blobs {
        let from = blob_path(source, &blob.digest)?;
        let to = blob_path(target, &blob.digest)?;
        verify_blob(&from, blob)?;
        copy_file(&from, &to, blob.byte_length)?;
        verify_blob(&to, blob)?;
    }
    sync_directory(target)?;
    Ok(())
}

fn copy_file(from: &Path, to: &Path, length: u64) -> Result<(), StoreError> {
    if !fs::symlink_metadata(from)?.file_type().is_file() {
        return Err(StoreError::Invalid);
    }
    let mut source = File::open(from)?.take(length);
    let mut target = OpenOptions::new().write(true).create_new(true).open(to)?;
    if std::io::copy(&mut source, &mut target)? != length {
        return Err(StoreError::Invalid);
    }
    target.sync_all()?;
    Ok(())
}

pub(crate) fn validate_database(
    conn: &Connection,
    expected_version: i64,
) -> Result<(), StoreError> {
    let application: i64 = conn.pragma_query_value(None, "application_id", |r| r.get(0))?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if application != APPLICATION_ID || version != expected_version {
        return Err(StoreError::Schema);
    }
    let check: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if check != "ok" {
        return Err(StoreError::Invalid);
    }
    let mut statement = conn.prepare("PRAGMA foreign_key_check")?;
    if statement.query([])?.next()?.is_some() {
        return Err(StoreError::Invalid);
    }
    Ok(())
}
