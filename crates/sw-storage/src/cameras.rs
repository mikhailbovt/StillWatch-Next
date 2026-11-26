use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sw_domain::{SourceScope, UnixMillis};
use sw_protocol::{
    AnalysisCapacity, CameraConfig, CameraPatch, CameraSampling, CameraSource, CapacityState,
    CaptureCapacity, Dimensions, IdempotencyKey, MAX_HEIGHT, MAX_WIDTH,
};
use uuid::Uuid;

use crate::{
    AnalysisAdmission, CameraSourceMeasurements, MAX_PAGE, Store, StoreError,
    db::{id, row_counter, sql_counter},
};

const IDEMPOTENCY_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1_000;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CameraRecord {
    pub id: Uuid,
    pub revision: u64,
    pub scene_revision: u64,
    pub epoch: u64,
    pub config: CameraConfig,
    pub dimensions: Option<Dimensions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_scene: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_measurements: Option<CameraSourceMeasurements>,
}

#[derive(Clone, Debug, Serialize)]
pub enum CameraAction {
    Create(CameraConfig),
    Patch {
        id: Uuid,
        revision: u64,
        patch: CameraPatch,
    },
    Delete {
        id: Uuid,
        revision: u64,
    },
}

impl CameraAction {
    fn operation(&self) -> &'static str {
        match self {
            Self::Create(_) => "camera.create",
            Self::Patch { .. } => "camera.patch",
            Self::Delete { .. } => "camera.delete",
        }
    }
}

#[derive(Clone, Debug)]
pub struct CameraCommand {
    pub actor: Uuid,
    pub key: IdempotencyKey,
    pub action: CameraAction,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CameraResult {
    pub camera: CameraRecord,
    pub deleted: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct CameraLimits {
    pub active: u32,
    pub raw_bytes: u32,
}

impl CameraLimits {
    pub fn required_raw_bytes(dims: Dimensions) -> Option<u32> {
        if !(1..=MAX_WIDTH).contains(&dims.width) || !(1..=MAX_HEIGHT).contains(&dims.height) {
            return None;
        }
        Some(dims.width * dims.height * 3 * 3)
    }
}

#[derive(Default)]
pub struct CameraAdmission {
    pub dimensions: Option<Dimensions>,
    pub reviewed_scene: Option<Vec<u8>>,
    pub source_measurements: Option<CameraSourceMeasurements>,
    pub analysis: Option<Arc<AnalysisAdmission>>,
}

impl Store {
    pub async fn current_camera_scopes(
        &self,
        scopes: Vec<SourceScope>,
    ) -> Result<Vec<SourceScope>, StoreError> {
        if scopes.len() > MAX_PAGE {
            return Err(StoreError::Invalid);
        }
        self.call(move |db| {
            let mut query = db.conn.prepare(
                "SELECT EXISTS(SELECT 1 FROM cameras WHERE id = ? AND source_epoch = ?
                 AND scene_revision = ? AND enabled = 1 AND deleted = 0)",
            )?;
            let mut current = Vec::new();
            for scope in scopes {
                if query.query_row(
                    params![
                        id(&scope.camera_id),
                        sql_counter(scope.epoch)?,
                        sql_counter(scope.scene_revision)?
                    ],
                    |row| row.get::<_, bool>(0),
                )? {
                    current.push(scope);
                }
            }
            Ok(current)
        })
        .await
    }

    pub async fn camera(&self, id: Uuid) -> Result<CameraRecord, StoreError> {
        self.call(move |db| get(&db.conn, id)).await
    }

    pub async fn camera_slot_available(
        &self,
        replacing: Option<Uuid>,
        limits: CameraLimits,
    ) -> Result<bool, StoreError> {
        self.call(move |db| {
            let (slots, _) = available_capture_capacity(&db.conn, replacing, limits)?;
            Ok(slots != 0)
        })
        .await
    }

    pub async fn camera_capacity(
        &self,
        dims: Dimensions,
        replacing: Option<Uuid>,
        limits: CameraLimits,
    ) -> Result<CaptureCapacity, StoreError> {
        self.call(move |db| {
            if let Some(camera) = replacing {
                get(&db.conn, camera)?;
            }
            capture_capacity(&db.conn, dims, replacing, limits)
        })
        .await
    }

    pub async fn analysis_capacity(
        &self,
        source: CameraSource,
        sampling: CameraSampling,
        measured: CameraSourceMeasurements,
        replacing: Option<Uuid>,
        profile: Arc<AnalysisAdmission>,
    ) -> Result<AnalysisCapacity, StoreError> {
        self.call(move |db| {
            let slots = available_compute_slots(&db.conn, replacing, &profile)?;
            Ok(profile.source_capacity(&source, sampling, Some(&measured), slots))
        })
        .await
    }

    pub async fn cameras(
        &self,
        after: Option<Uuid>,
        limit: usize,
    ) -> Result<(Vec<CameraRecord>, Option<Uuid>), StoreError> {
        self.cameras_visible(after, limit, None).await
    }

    pub async fn cameras_visible(
        &self,
        after: Option<Uuid>,
        limit: usize,
        allowed: Option<Vec<Uuid>>,
    ) -> Result<(Vec<CameraRecord>, Option<Uuid>), StoreError> {
        if !(1..=MAX_PAGE).contains(&limit) {
            return Err(StoreError::Invalid);
        }
        self.call(move |db| {
            let allowed = camera_filter(allowed)?;
            let mut statement = db.conn.prepare(
                "SELECT id, revision, scene_revision, source_epoch, config, width, height, reviewed_scene, source_measurements FROM cameras
                 WHERE deleted = 0 AND (?1 IS NULL OR id > ?1)
                 AND (?3 IS NULL OR lower(hex(id)) IN (SELECT value FROM json_each(?3))) ORDER BY id LIMIT ?2",
            )?;
            let mut cameras = statement.query_map(params![after.as_ref().map(id), limit as i64 + 1, allowed], record)?.collect::<Result<Vec<_>, _>>()?;
            let next = if cameras.len() > limit { cameras.pop(); cameras.last().map(|camera| camera.id) } else { None };
            Ok((cameras, next))
        }).await
    }

    pub async fn enabled_cameras(&self) -> Result<Vec<CameraRecord>, StoreError> {
        self.call(|db| {
            let mut statement = db.conn.prepare(
                "SELECT id, revision, scene_revision, source_epoch, config, width, height, reviewed_scene, source_measurements FROM cameras WHERE enabled = 1 AND deleted = 0 ORDER BY id",
            )?;
            Ok(statement.query_map([], record)?.collect::<Result<Vec<_>, _>>()?)
        }).await
    }

    pub async fn cached_camera(
        &self,
        command: CameraCommand,
        now: UnixMillis,
    ) -> Result<Option<CameraResult>, StoreError> {
        self.call(move |db| cached(&db.conn, &command, now)).await
    }

    pub async fn mutate_camera(
        &self,
        command: CameraCommand,
        dimensions: Option<Dimensions>,
        reviewed_scene: Option<Vec<u8>>,
        limits: CameraLimits,
        now: UnixMillis,
    ) -> Result<CameraResult, StoreError> {
        self.mutate_camera_admitted(
            command,
            CameraAdmission {
                dimensions,
                reviewed_scene,
                ..CameraAdmission::default()
            },
            limits,
            now,
        )
        .await
    }

    pub async fn mutate_camera_admitted(
        &self,
        command: CameraCommand,
        evaluated: CameraAdmission,
        limits: CameraLimits,
        now: UnixMillis,
    ) -> Result<CameraResult, StoreError> {
        self.call(move |db| {
            let CameraAdmission { dimensions, reviewed_scene, source_measurements, analysis } = evaluated;
            let tx = db.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if let Some(result) = cached(&tx, &command, now)? { return Ok(result); }
            let (camera, deleted) = match &command.action {
                CameraAction::Create(config) => (CameraRecord {
                    id: Uuid::new_v4(), revision: 1, scene_revision: 1, epoch: 0,
                    config: config.clone(), dimensions, reviewed_scene, source_measurements,
                }, false),
                CameraAction::Patch { id, revision, patch } => {
                    let previous = check_revision(&tx, *id, *revision)?;
                    let config = patch.merge(previous.config.clone());
                    let scene_changed = config.source != previous.config.source
                        || config.privacy_masks != previous.config.privacy_masks
                        || (dimensions.is_some() && dimensions != previous.dimensions)
                        || patch.controlled_fire_regions.is_some();
                    let reviewed_scene = if scene_changed { reviewed_scene } else { previous.reviewed_scene.clone() };
                    let source_measurements = if config.enabled && config.source == previous.config.source
                        && dimensions == previous.dimensions {
                        source_measurements.or_else(|| previous.source_measurements.clone())
                    } else {
                        source_measurements
                    };
                    // Invalidate frames when consent, sampling or availability changes.
                    // The commit precedes the capture worker's restart.
                    let epoch = if config.cloud_consent != previous.config.cloud_consent
                        || config.sampling != previous.config.sampling
                        || config.priority != previous.config.priority
                        || (!config.enabled && previous.config.enabled) {
                        next(previous.epoch)?
                    } else {
                        previous.epoch
                    };
                    (CameraRecord {
                        config, dimensions, reviewed_scene, source_measurements, epoch, revision: next(previous.revision)?,
                        scene_revision: if scene_changed { next(previous.scene_revision)? } else { previous.scene_revision },
                        ..previous
                    }, false)
                }
                CameraAction::Delete { id, revision } => {
                    let mut camera = check_revision(&tx, *id, *revision)?;
                    camera.config.enabled = false;
                    camera.revision = next(camera.revision)?;
                    camera.epoch = next(camera.epoch)?;
                    (camera, true)
                }
            };
            if !camera.config.valid() { return Err(StoreError::Invalid); }
            if camera.reviewed_scene.as_ref().is_some_and(|value| value.len() != 48) {
                return Err(StoreError::Invalid);
            }
            let reserved = if camera.config.enabled {
                let dims = camera.dimensions.ok_or(StoreError::Invalid)?;
                let capacity = capture_capacity(&tx, dims, Some(camera.id), limits)?;
                if !capacity.fits {
                    return Err(StoreError::CameraCapacity);
                }
                if let Some(profile) = analysis {
                    let available = available_compute_slots(&tx, Some(camera.id), &profile)?;
                    let capacity = profile.camera_capacity(&camera.config, camera.source_measurements.as_ref(), available);
                    if capacity.state != CapacityState::Verified {
                        return Err(StoreError::AnalysisCapacity {
                            reason: format!("{}. Profile {} admits up to {} cameras at Pose {} fps and Fire {} fps.",
                                capacity.reason, profile.id, profile.workload.cameras,
                                profile.workload.sampling.pose_fps, profile.workload.sampling.fire_fps),
                        });
                    }
                }
                capacity.required_raw_bytes
            } else { 0 };
            let config = serde_json::to_string(&camera.config)?;
            let revision = sql_counter(camera.revision)?;
            let measurements = camera.source_measurements.as_ref().map(serde_json::to_string).transpose()?;
            tx.execute(
                "INSERT INTO cameras(id, revision, scene_revision, source_epoch, enabled, deleted, bytes_reserved, config, width, height, reviewed_scene, source_measurements)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET revision = excluded.revision, scene_revision = excluded.scene_revision,
                   source_epoch = excluded.source_epoch,
                   enabled = excluded.enabled, deleted = excluded.deleted, bytes_reserved = excluded.bytes_reserved,
                   config = excluded.config, width = excluded.width, height = excluded.height, reviewed_scene = excluded.reviewed_scene,
                   source_measurements = excluded.source_measurements",
                params![id(&camera.id), revision, sql_counter(camera.scene_revision)?, sql_counter(camera.epoch)?, camera.config.enabled,
                    deleted, reserved, config, camera.dimensions.map(|d| d.width), camera.dimensions.map(|d| d.height),
                    camera.reviewed_scene, measurements],
            )?;
            tx.execute("INSERT INTO camera_config_versions VALUES (?, ?, ?, ?, ?, ?)", params![id(&camera.id), revision, id(&command.actor), now, config, deleted])?;
            if !camera.config.enabled {
                tx.execute(
                    "UPDATE health_events SET ended_at = max(opened_at, ?1)
                     WHERE ended_at IS NULL AND ifnull(camera_id, X'') = ?2",
                    params![now, id(&camera.id)],
                )?;
            }
            let result = CameraResult { camera, deleted };
            let response = serde_json::to_string(&result)?;
            let hash = action_hash(&command.action)?;
            tx.execute(
                "DELETE FROM idempotency_records WHERE actor_id = ? AND operation = ? AND key = ? AND expires_at <= ?",
                params![id(&command.actor), command.action.operation(), command.key.as_str(), now],
            )?;
            tx.execute(
                "INSERT INTO idempotency_records(actor_id, operation, key, request_hash, result, expires_at) VALUES (?, ?, ?, ?, ?, ?)",
                params![id(&command.actor), command.action.operation(), command.key.as_str(), hash.as_slice(), response, now.saturating_add(IDEMPOTENCY_TTL_MS)],
            )?;
            tx.commit()?;
            db.notify.send_replace(());
            Ok(result)
        }).await
    }

    pub async fn bump_camera_epoch(
        &self,
        camera: Uuid,
        revision: u64,
    ) -> Result<CameraRecord, StoreError> {
        self.call(move |db| {
            let tx = db
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut current = check_revision(&tx, camera, revision)?;
            if !current.config.enabled {
                return Err(StoreError::Invalid);
            }
            current.epoch = next(current.epoch)?;
            tx.execute(
                "UPDATE cameras SET source_epoch = ? WHERE id = ?",
                params![sql_counter(current.epoch)?, id(&camera)],
            )?;
            tx.commit()?;
            Ok(current)
        })
        .await
    }

    pub async fn invalidate_camera_scene(
        &self,
        camera: Uuid,
        revision: u64,
    ) -> Result<CameraRecord, StoreError> {
        self.call(move |db| {
            let tx = db.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let mut current = check_revision(&tx, camera, revision)?;
            if !current.config.enabled {
                return Err(StoreError::Invalid);
            }
            current.epoch = next(current.epoch)?;
            current.scene_revision = next(current.scene_revision)?;
            current.reviewed_scene = None;
            tx.execute(
                "UPDATE cameras SET source_epoch = ?, scene_revision = ?, reviewed_scene = NULL WHERE id = ?",
                params![sql_counter(current.epoch)?, sql_counter(current.scene_revision)?, id(&camera)],
            )?;
            tx.commit()?;
            Ok(current)
        }).await
    }
}

pub(crate) fn camera_filter(allowed: Option<Vec<Uuid>>) -> Result<Option<String>, StoreError> {
    allowed
        .map(|ids| {
            if ids.len() > 128 {
                return Err(StoreError::Invalid);
            }
            let hex: Vec<_> = ids.into_iter().map(|id| id.simple().to_string()).collect();
            Ok(serde_json::to_string(&hex)?)
        })
        .transpose()
}

fn capture_capacity(
    conn: &Connection,
    dims: Dimensions,
    replacing: Option<Uuid>,
    limits: CameraLimits,
) -> Result<CaptureCapacity, StoreError> {
    let required_raw_bytes = CameraLimits::required_raw_bytes(dims).ok_or(StoreError::Invalid)?;
    let (available_camera_slots, available_raw_bytes) =
        available_capture_capacity(conn, replacing, limits)?;
    Ok(CaptureCapacity {
        fits: available_camera_slots > 0 && available_raw_bytes >= required_raw_bytes,
        available_camera_slots,
        available_raw_bytes,
        required_raw_bytes,
    })
}

fn available_capture_capacity(
    conn: &Connection,
    replacing: Option<Uuid>,
    limits: CameraLimits,
) -> Result<(u32, u32), StoreError> {
    let (active, occupied) = conn.query_row(
        "SELECT count(*), coalesce(sum(bytes_reserved), 0) FROM cameras
         WHERE enabled = 1 AND deleted = 0 AND (?1 IS NULL OR id != ?1)",
        [replacing.as_ref().map(id)],
        |row| Ok((row_counter(row, 0)?, row_counter(row, 1)?)),
    )?;
    let available_camera_slots = limits
        .active
        .saturating_sub(u32::try_from(active).unwrap_or(u32::MAX));
    let available_raw_bytes = limits
        .raw_bytes
        .saturating_sub(u32::try_from(occupied).unwrap_or(u32::MAX));
    Ok((available_camera_slots, available_raw_bytes))
}

fn available_compute_slots(
    conn: &Connection,
    replacing: Option<Uuid>,
    profile: &AnalysisAdmission,
) -> Result<u32, StoreError> {
    let mut query = conn.prepare(
        "SELECT config, source_measurements FROM cameras
         WHERE enabled = 1 AND deleted = 0 AND (?1 IS NULL OR id != ?1)",
    )?;
    let mut rows = query.query([replacing.as_ref().map(id)])?;
    let mut active = 0;
    while let Some(row) = rows.next()? {
        let config: CameraConfig =
            serde_json::from_str(row.get_ref(0)?.as_str().map_err(|_| StoreError::Schema)?)?;
        let measured: Option<String> = row.get(1)?;
        let measured = measured.as_deref().map(serde_json::from_str).transpose()?;
        if profile.matches_camera(&config, measured.as_ref()) {
            active += 1;
            if active == profile.workload.cameras {
                return Ok(0);
            }
        }
    }
    Ok(profile.workload.cameras.saturating_sub(active))
}

pub(crate) fn get(conn: &Connection, camera: Uuid) -> Result<CameraRecord, StoreError> {
    conn.query_row("SELECT id, revision, scene_revision, source_epoch, config, width, height, reviewed_scene, source_measurements FROM cameras WHERE id = ? AND deleted = 0", [id(&camera)], record)
        .optional()?.ok_or(StoreError::NotFound)
}

fn record(row: &Row<'_>) -> rusqlite::Result<CameraRecord> {
    let camera = row.get::<_, Vec<u8>>(0)?;
    let config: String = row.get(4)?;
    let width: Option<u32> = row.get(5)?;
    let height: Option<u32> = row.get(6)?;
    let measurements: Option<String> = row.get(8)?;
    Ok(CameraRecord {
        id: Uuid::from_slice(&camera).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Blob, Box::new(e))
        })?,
        revision: row_counter(row, 1)?,
        scene_revision: row_counter(row, 2)?,
        epoch: row_counter(row, 3)?,
        config: serde_json::from_str(&config).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
        })?,
        dimensions: width
            .zip(height)
            .map(|(width, height)| Dimensions { width, height }),
        reviewed_scene: row.get(7)?,
        source_measurements: measurements
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    8,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
    })
}

fn check_revision(conn: &Connection, id: Uuid, revision: u64) -> Result<CameraRecord, StoreError> {
    let camera = get(conn, id)?;
    if camera.revision != revision {
        return Err(StoreError::Revision {
            current: camera.revision,
        });
    }
    Ok(camera)
}

fn cached(
    conn: &Connection,
    command: &CameraCommand,
    now: UnixMillis,
) -> Result<Option<CameraResult>, StoreError> {
    let saved = conn.query_row(
        "SELECT request_hash, result FROM idempotency_records WHERE actor_id = ? AND operation = ? AND key = ? AND expires_at > ?",
        params![id(&command.actor), command.action.operation(), command.key.as_str(), now],
        |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
    ).optional()?;
    let Some((hash, state)) = saved else {
        return Ok(None);
    };
    if hash != action_hash(&command.action)? {
        return Err(StoreError::Idempotency);
    }
    Ok(Some(serde_json::from_str(&state)?))
}

fn action_hash(action: &CameraAction) -> Result<[u8; 32], StoreError> {
    Ok(Sha256::digest(serde_json::to_vec(action)?).into())
}

fn next(value: u64) -> Result<u64, StoreError> {
    value.checked_add(1).ok_or(StoreError::Invalid)
}
