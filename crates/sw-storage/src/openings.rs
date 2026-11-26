use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sw_domain::{Box2d, NewIncident, SceneSignature};
use sw_protocol::LocalFacts;
use uuid::Uuid;

use crate::{Identity, StoreError, db::id, utc_ms, validate_hazard_observation};

const JOURNAL_FILE: &str = "accepted-openings.sqlite3";
const MAX_OPENINGS: i64 = 4096;
const MAX_ENTRY_BYTES: usize = 8192;

/// A policy opening emitted after Core has accepted the frame's source scope.
/// It contains observation metadata only. Evidence images have a separate lifecycle.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpeningDecision {
    pub(crate) incident: NewIncident,
    pub(crate) facts: LocalFacts,
    pub(crate) region: Box2d,
    pub(crate) signature: Option<Vec<u8>>,
}

impl OpeningDecision {
    pub fn new(
        incident: NewIncident,
        facts: LocalFacts,
        region: Box2d,
        signature: Option<SceneSignature>,
    ) -> Result<Self, StoreError> {
        validate_hazard_observation(&incident, facts, region)?;
        if incident.id.is_nil()
            || incident.analysis_origin.is_none()
            || incident.observed_host_utc.is_none()
        {
            return Err(StoreError::Invalid);
        }
        sw_domain::Incident::create(incident.clone(), utc_ms())?;
        Ok(Self {
            incident,
            facts,
            region,
            signature: signature.map(|signature| signature.0.to_vec()),
        })
    }
}

#[derive(Clone)]
pub(crate) struct OpeningJournal(Arc<Mutex<Connection>>);

impl OpeningJournal {
    pub fn open(root: &Path, identity: Identity) -> Result<Self, StoreError> {
        let path = root.join(JOURNAL_FILE);
        match path.symlink_metadata() {
            Ok(metadata) if !metadata.file_type().is_file() => return Err(StoreError::Invalid),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let mut db = Connection::open(path)?;
        db.busy_timeout(Duration::from_millis(200))?;
        db.execute_batch(
            "PRAGMA journal_mode = DELETE;
             PRAGMA synchronous = EXTRA;
             PRAGMA trusted_schema = OFF;
             PRAGMA max_page_count = 4096;",
        )?;
        let tx = db.transaction()?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS journal_identity (
                 singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                 instance BLOB NOT NULL, generation BLOB NOT NULL
             ) STRICT;
             CREATE TABLE IF NOT EXISTS openings (
                 sequence INTEGER PRIMARY KEY,
                 id BLOB NOT NULL UNIQUE,
                 accepted_utc INTEGER NOT NULL,
                 body TEXT NOT NULL
             ) STRICT;",
        )?;
        tx.execute(
            "INSERT INTO journal_identity VALUES(1, ?, ?) ON CONFLICT DO NOTHING",
            params![id(&identity.instance_id), id(&identity.generation)],
        )?;
        let matches = tx.query_row(
            "SELECT instance = ? AND generation = ? FROM journal_identity WHERE singleton = 1",
            params![id(&identity.instance_id), id(&identity.generation)],
            |row| row.get::<_, bool>(0),
        )?;
        if !matches {
            return Err(StoreError::InUse);
        }
        tx.commit()?;
        Ok(Self(Arc::new(Mutex::new(db))))
    }

    pub fn insert(&self, openings: &[OpeningDecision]) -> Result<(), StoreError> {
        let mut db = self.lock()?;
        let tx = db.transaction()?;
        let mut count = tx.query_row("SELECT count(*) FROM openings", [], |row| {
            row.get::<_, i64>(0)
        })?;
        let now = utc_ms();
        for opening in openings {
            let key = id(&opening.incident.id);
            let body = serde_json::to_string(opening)?;
            if body.len() > MAX_ENTRY_BYTES {
                return Err(StoreError::Invalid);
            }
            let previous = tx
                .query_row("SELECT body FROM openings WHERE id = ?", [key], |row| {
                    row.get::<_, String>(0)
                })
                .optional()?;
            if let Some(previous) = previous {
                if previous != body {
                    return Err(StoreError::IncidentConflict);
                }
                continue;
            }
            if count >= MAX_OPENINGS {
                return Err(StoreError::DatabaseCapacity);
            }
            tx.execute(
                "INSERT INTO openings(id, accepted_utc, body) VALUES(?, ?, ?)",
                params![key, now, body],
            )?;
            count += 1;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get(&self, incident: Uuid) -> Result<Option<OpeningDecision>, StoreError> {
        let body = self
            .lock()?
            .query_row(
                "SELECT body FROM openings WHERE id = ?",
                [id(&incident)],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        body.map(|body| serde_json::from_str(&body).map_err(Into::into))
            .transpose()
    }

    pub fn pending(&self) -> Result<VecDeque<Uuid>, StoreError> {
        let db = self.lock()?;
        let mut query = db.prepare("SELECT id FROM openings ORDER BY sequence LIMIT ?")?;
        let rows = query.query_map([MAX_OPENINGS], |row| row.get::<_, Vec<u8>>(0))?;
        rows.map(|row| Uuid::from_slice(&row?).map_err(|_| StoreError::Invalid))
            .collect()
    }

    pub fn acknowledge(&self, incident: Uuid) -> Result<(), StoreError> {
        self.lock()?
            .execute("DELETE FROM openings WHERE id = ?", [id(&incident)])?;
        Ok(())
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, StoreError> {
        self.0.lock().map_err(|_| StoreError::Unavailable)
    }
}
