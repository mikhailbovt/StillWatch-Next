use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use rusqlite::{Connection, ErrorCode};

use crate::StoreError;

const MIB: i64 = 1024 * 1024;
pub const DEFAULT_DATABASE_QUOTA_BYTES: u64 = 2 * 1024 * MIB as u64;
const WAL_CHECKPOINT_BYTES: i64 = 16 * MIB;
const CHECKPOINT_RETRY: Duration = Duration::from_secs(2);
pub(crate) const BUSY_TIMEOUT: Duration = Duration::from_secs(3);
const CONTENDED_TIMEOUT: Duration = Duration::from_millis(50);

pub(crate) fn valid_quota(bytes: u64) -> bool {
    (4 * MIB as u64..=i64::MAX as u64).contains(&bytes)
}

pub(crate) struct DatabaseBudget {
    page_limit: i64,
    warning_pages: i64,
    wal: PathBuf,
    retry_at: Option<Instant>,
    busy_timeout: Duration,
}

impl DatabaseBudget {
    pub(crate) fn configure(
        conn: &Connection,
        file: &Path,
        quota: u64,
    ) -> Result<Self, StoreError> {
        let page_size: i64 = conn.pragma_query_value(None, "page_size", |row| row.get(0))?;
        let current_limit: i64 =
            conn.pragma_query_value(None, "max_page_count", |row| row.get(0))?;
        let requested =
            (i64::try_from(quota).map_err(|_| StoreError::Invalid)? / page_size).min(current_limit);
        let actual: i64 =
            conn.pragma_update_and_check(None, "max_page_count", requested, |row| row.get(0))?;
        conn.pragma_update(None, "journal_size_limit", WAL_CHECKPOINT_BYTES)?;
        let page_limit = actual.min(requested);
        let warning_pages = (page_limit / 64).min(16 * MIB / page_size).max(1);
        let mut wal = file.as_os_str().to_owned();
        wal.push("-wal");
        Ok(Self {
            page_limit,
            warning_pages,
            wal: wal.into(),
            retry_at: None,
            busy_timeout: BUSY_TIMEOUT,
        })
    }

    pub(crate) fn is_low(&self, conn: &Connection) -> Result<bool, StoreError> {
        let allocated: i64 = conn.pragma_query_value(None, "page_count", |row| row.get(0))?;
        let reusable: i64 = conn.pragma_query_value(None, "freelist_count", |row| row.get(0))?;
        Ok(allocated > self.page_limit
            || allocated.saturating_sub(reusable) >= self.page_limit - self.warning_pages)
    }

    pub(crate) fn ready(&self, conn: &Connection) -> Result<(), StoreError> {
        if !self.writable() || self.is_low(conn)? {
            return Err(StoreError::DatabaseCapacity);
        }
        Ok(())
    }

    pub(crate) fn writable(&self) -> bool {
        self.retry_at.is_none()
    }

    pub(crate) fn should_retry(&self) -> bool {
        self.retry_at.is_some_and(|due| Instant::now() >= due)
    }

    pub(crate) fn observe_operation<T>(
        &mut self,
        conn: &Connection,
        outcome: &Result<T, StoreError>,
        changed: bool,
    ) {
        // Give one writer the normal grace period. Repeating that wait for every
        // queued write also blocks reads of already committed data on this worker.
        let timeout = match outcome {
            Err(StoreError::Sql(error))
                if matches!(
                    error.sqlite_error_code(),
                    Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
                ) =>
            {
                CONTENDED_TIMEOUT
            }
            Ok(_) if changed => BUSY_TIMEOUT,
            _ => return,
        };
        if timeout != self.busy_timeout && conn.busy_timeout(timeout).is_ok() {
            self.busy_timeout = timeout;
        }
    }

    pub(crate) fn check_wal(&mut self, conn: &Connection) -> Result<(), StoreError> {
        if self.retry_at.is_some() && !self.should_retry() {
            return Ok(());
        }
        let finished = match fs::metadata(&self.wal) {
            Ok(meta) if meta.len() >= WAL_CHECKPOINT_BYTES as u64 => {
                try_checkpoint(conn, self.busy_timeout).unwrap_or(false)
            }
            Ok(_) => true,
            Err(error) => error.kind() == io::ErrorKind::NotFound,
        };
        let blocked = self.retry_at.is_some();
        let must_block = !finished;
        if blocked != must_block {
            conn.pragma_update(None, "query_only", must_block)?;
            if blocked {
                eprintln!("StillWatch metadata writes resumed after SQLite checkpoint");
            } else {
                eprintln!(
                    "StillWatch metadata writes suspended because the SQLite WAL is unavailable or over budget"
                );
            }
        }
        self.retry_at = must_block.then(|| Instant::now() + CHECKPOINT_RETRY);
        Ok(())
    }

    pub(crate) fn classify(&self, error: StoreError) -> StoreError {
        match &error {
            StoreError::Sql(error)
                if error.sqlite_error_code() == Some(ErrorCode::DiskFull)
                    || (self.retry_at.is_some()
                        && error.sqlite_error_code() == Some(ErrorCode::ReadOnly)) =>
            {
                StoreError::DatabaseCapacity
            }
            _ => error,
        }
    }
}

fn try_checkpoint(conn: &Connection, busy_timeout: Duration) -> Result<bool, rusqlite::Error> {
    // An external reader must not make the Core writer wait on a checkpoint.
    conn.busy_timeout(Duration::ZERO)?;
    let result = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
        Ok(row.get::<_, i64>(0)? == 0)
    });
    conn.busy_timeout(busy_timeout)?;
    result
}
