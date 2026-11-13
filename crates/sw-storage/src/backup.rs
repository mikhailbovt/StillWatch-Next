use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
    time::Duration,
};

use rusqlite::{Connection, OpenFlags, backup::Backup};
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
    pub fn backup(&mut self, destination: &Path) -> Result<(), StoreError> {
        let parent = fresh_destination(destination)?;
        let stage = tempfile::Builder::new()
            .prefix(".stillwatch-backup-")
            .tempdir_in(parent)?;
        let db_path = stage.path().join(DATABASE_FILE);
        let mut target = Connection::open(&db_path)?;
        Backup::new(&self.conn, &mut target)?.run_to_completion(
            128,
            Duration::from_millis(1),
            None,
        )?;
        target.execute_batch("PRAGMA journal_mode = DELETE; PRAGMA trusted_schema = OFF;")?;
        validate_database(&target, SCHEMA_VERSION)?;
        let blobs = available_blobs(&target)?;
        let target_blobs = stage.path().join("blobs");
        private_dir(&target_blobs)?;
        copy_blobs(&blobs, &self.blobs, &target_blobs)?;
        drop(target);
        OpenOptions::new().write(true).open(&db_path)?.sync_all()?;
        let manifest = Manifest {
            schema_version: SCHEMA_VERSION,
            instance_id: self.identity.instance_id,
            generation: self.identity.generation,
            database_sha256: file_digest(&db_path)?,
            blobs,
        };
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

    let stage = tempfile::Builder::new()
        .prefix(".stillwatch-restore-")
        .tempdir_in(parent)?;
    let restored_db = stage.path().join(DATABASE_FILE);
    copy_file(&backup.join(DATABASE_FILE), &restored_db)?;
    if file_digest(&restored_db)? != manifest.database_sha256 {
        return Err(StoreError::Invalid);
    }
    let mut conn = Connection::open_with_flags(&restored_db, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    conn.execute_batch(
        "PRAGMA synchronous = FULL; PRAGMA trusted_schema = OFF; PRAGMA foreign_keys = ON;",
    )?;
    validate_database(&conn, manifest.schema_version)?;
    let original = read_identity(&conn)?;
    if original.instance_id != manifest.instance_id
        || original.generation != manifest.generation
        || available_blobs(&conn)? != manifest.blobs
    {
        return Err(StoreError::Invalid);
    }
    let blobs = stage.path().join("blobs");
    private_dir(&blobs)?;
    copy_blobs(&manifest.blobs, &backup.join("blobs"), &blobs)?;
    migrate(&mut conn)?;
    let generation = Uuid::new_v4();
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
    tx.execute("DELETE FROM device_sessions", [])?;
    tx.execute("DELETE FROM retired_refresh_credentials", [])?;
    tx.execute(
        "UPDATE trusted_devices SET revoked_at = COALESCE(revoked_at, ?)",
        [crate::evidence::now_millis()?],
    )?;
    tx.commit()?;
    drop(conn);
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
        copy_file(&from, &to)?;
        verify_blob(&to, blob)?;
    }
    sync_directory(target)?;
    Ok(())
}

fn copy_file(from: &Path, to: &Path) -> Result<(), StoreError> {
    if !fs::symlink_metadata(from)?.file_type().is_file() {
        return Err(StoreError::Invalid);
    }
    let mut source = File::open(from)?;
    let mut target = OpenOptions::new().write(true).create_new(true).open(to)?;
    std::io::copy(&mut source, &mut target)?;
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
