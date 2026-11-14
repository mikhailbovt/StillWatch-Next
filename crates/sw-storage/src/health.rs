use rusqlite::{TransactionBehavior, params};
use sw_domain::UnixMillis;
use sw_protocol::{Counter, HealthEvent, HealthEventPage, UtcTimestamp};
use uuid::Uuid;

use crate::{
    MAX_CHANGES, MAX_PAGE, Store, StoreError,
    cameras::camera_filter,
    db::{id, row_counter, sql_counter},
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum HealthIssue {
    CameraOffline,
    CameraFrozen,
    CameraCapacityExceeded,
    InvalidSourceClock,
    HostClockDiscontinuity,
    InferenceUnavailable,
    InferenceBelowTarget,
    ModelPackUnverified,
    SemanticUnavailable,
    SemanticBudgetExhausted,
    DiskSpaceLow,
    DiskSpaceUnknown,
    DatabaseBudgetLow,
}

impl HealthIssue {
    fn code(self) -> &'static str {
        match self {
            Self::CameraOffline => "camera_offline",
            Self::CameraFrozen => "camera_frozen",
            Self::CameraCapacityExceeded => "camera_capacity_exceeded",
            Self::InvalidSourceClock => "invalid_source_clock",
            Self::HostClockDiscontinuity => "host_clock_discontinuity",
            Self::InferenceUnavailable => "inference_unavailable",
            Self::InferenceBelowTarget => "inference_below_target",
            Self::ModelPackUnverified => "model_pack_unverified",
            Self::SemanticUnavailable => "semantic_unavailable",
            Self::SemanticBudgetExhausted => "semantic_budget_exhausted",
            Self::DiskSpaceLow => "disk_space_low",
            Self::DiskSpaceUnknown => "disk_space_unknown",
            Self::DatabaseBudgetLow => "database_budget_low",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct HealthObservation {
    /// Capture issues apply only to the configuration revision that was observed.
    pub camera: Option<(Uuid, u64)>,
    pub issue: HealthIssue,
    pub active: bool,
}

impl HealthObservation {
    pub fn global(issue: HealthIssue, active: bool) -> Self {
        Self {
            camera: None,
            issue,
            active,
        }
    }
}

impl Store {
    /// Record positively observed conditions. Missing observations leave intervals open.
    pub async fn record_health(
        &self,
        observations: Vec<HealthObservation>,
        now: UnixMillis,
    ) -> Result<(), StoreError> {
        self.call(move |db| {
            let tx = db.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut opened = 0;
            {
                let mut check_camera = tx.prepare(
                    "SELECT EXISTS(SELECT 1 FROM cameras WHERE id = ? AND revision = ?
                         AND enabled = 1 AND deleted = 0)",
                )?;
                let mut add = tx.prepare(
                    "INSERT INTO health_events(camera_id, code, opened_at)
                     SELECT ?1, ?2, ?3 WHERE NOT EXISTS (
                         SELECT 1 FROM health_events WHERE ifnull(camera_id, X'') = ifnull(?1, X'')
                         AND code = ?2 AND ended_at IS NULL)",
                )?;
                let mut end = tx.prepare(
                    "UPDATE health_events SET ended_at = max(opened_at, ?3)
                     WHERE ifnull(camera_id, X'') = ifnull(?1, X'') AND code = ?2 AND ended_at IS NULL",
                )?;
                for observation in observations {
                    if let Some((camera, revision)) = observation.camera
                        && !check_camera.query_row(params![id(&camera), sql_counter(revision)?],
                            |row| row.get::<_, bool>(0))?
                    {
                        continue;
                    }
                    let arguments = params![
                        observation.camera.as_ref().map(|(camera, _)| id(camera)),
                        observation.issue.code(),
                        now
                    ];
                    if observation.active {
                        opened += add.execute(arguments)?;
                    } else {
                        end.execute(arguments)?;
                    }
                }
            }
            if opened > 0 {
                tx.execute(
                    "DELETE FROM health_events WHERE sequence <= ? AND ended_at IS NOT NULL",
                    [tx.last_insert_rowid() - MAX_CHANGES as i64],
                )?;
            }
            tx.commit()?;
            Ok(())
        }).await
    }

    pub async fn health_events(
        &self,
        before: Option<Counter>,
        limit: usize,
        active_only: bool,
        allowed: Option<Vec<Uuid>>,
    ) -> Result<HealthEventPage, StoreError> {
        if !(1..=MAX_PAGE).contains(&limit) || before == Some(Counter(0)) {
            return Err(StoreError::Invalid);
        }
        let before = before.map(|sequence| sql_counter(sequence.0)).transpose()?;
        let allowed = camera_filter(allowed)?;
        self.call(move |db| {
            let mut statement = db.conn.prepare(if active_only {
                "SELECT sequence, camera_id, code, opened_at, ended_at FROM health_events
                 WHERE ended_at IS NULL AND (?1 IS NULL OR sequence < ?1)
                   AND (?2 IS NULL OR camera_id IS NULL OR lower(hex(camera_id)) IN (SELECT value FROM json_each(?2)))
                 ORDER BY sequence DESC LIMIT ?3"
            } else {
                "SELECT sequence, camera_id, code, opened_at, ended_at FROM health_events
                 WHERE (?1 IS NULL OR sequence < ?1)
                   AND (?2 IS NULL OR camera_id IS NULL OR lower(hex(camera_id)) IN (SELECT value FROM json_each(?2)))
                 ORDER BY sequence DESC LIMIT ?3"
            })?;
            let mut events = statement
                .query_map(params![before, allowed, limit as i64 + 1], |row| {
                    Ok((
                        row_counter(row, 0)?,
                        row.get::<_, Option<Vec<u8>>>(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get::<_, Option<i64>>(4)?,
                    ))
                })?
                .map(|row| {
                    let (sequence, camera_id, code, opened_at, ended_at) = row?;
                    let timestamp =
                        |ms| UtcTimestamp::from_unix_millis(ms).map_err(|_| StoreError::Schema);
                    Ok(HealthEvent {
                        sequence: Counter(sequence),
                        camera_id: camera_id.map(|bytes| Uuid::from_slice(&bytes))
                            .transpose().map_err(|_| StoreError::Schema)?,
                        code,
                        opened_at: timestamp(opened_at)?,
                        ended_at: ended_at.map(timestamp).transpose()?,
                    })
                })
                .collect::<Result<Vec<_>, StoreError>>()?;
            let next = if events.len() > limit {
                events.pop();
                events.last().map(|event| event.sequence)
            } else {
                None
            };
            Ok(HealthEventPage { events, next })
        }).await
    }
}
