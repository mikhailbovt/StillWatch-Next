use std::sync::Arc;

use axum::{
    Extension, Json,
    body::Body,
    extract::{
        Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use sw_engine::CameraRegistry;
use sw_protocol::{
    Camera, CameraConfig, CameraPage, CameraPatch, Counter, IdempotencyKey,
    MAX_REPLAY_UPLOAD_BYTES, OnvifDevice, OnvifProfile, OnvifProfilesRequest, OnvifStream,
    OnvifStreamRequest, SourceProbe, SourceProbeRequest,
};
use sw_storage::{CameraAction, CameraCommand, CameraRecord};
use uuid::Uuid;

use crate::{Api, auth::Principal, errors::ApiError};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pagination {
    limit: Option<usize>,
    after: Option<Uuid>,
}

pub async fn list(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    query: Result<Query<Pagination>, QueryRejection>,
) -> Result<Json<CameraPage>, ApiError> {
    let Query(query) = query.map_err(|_| ApiError::invalid("Invalid camera list query"))?;
    let (records, next) = state
        .store
        .cameras_visible(
            query.after,
            query.limit.unwrap_or(100),
            principal.allowed_cameras(),
        )
        .await?;
    let registry = registry(&state)?;
    Ok(Json(CameraPage {
        cameras: records
            .into_iter()
            .map(|record| view(&state, registry, record))
            .collect(),
        next,
    }))
}

pub async fn detail(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let id = resource_id(&id)?;
    principal.camera(id)?;
    let camera = state.store.camera(id).await?;
    response(StatusCode::OK, view(&state, registry(&state)?, camera))
}

pub async fn create(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
    body: Result<Json<CameraConfig>, JsonRejection>,
) -> Result<Response, ApiError> {
    let config = json(body)?;
    let saved = mutate(&state, &principal, headers, CameraAction::Create(config)).await?;
    response(
        StatusCode::CREATED,
        view(&state, registry(&state)?, saved.camera),
    )
}

pub async fn patch(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<CameraPatch>, JsonRejection>,
) -> Result<Response, ApiError> {
    let action = CameraAction::Patch {
        id: resource_id(&id)?,
        revision: expected_revision(&headers)?,
        patch: json(body)?,
    };
    let saved = mutate(&state, &principal, headers, action).await?;
    response(
        StatusCode::OK,
        view(&state, registry(&state)?, saved.camera),
    )
}

pub async fn remove(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let action = CameraAction::Delete {
        id: resource_id(&id)?,
        revision: expected_revision(&headers)?,
    };
    mutate(&state, &principal, headers, action).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn probe(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    body: Result<Json<SourceProbeRequest>, JsonRejection>,
) -> Result<Json<SourceProbe>, ApiError> {
    principal.owner()?;
    let request = json(body)?;
    Ok(Json(
        registry(&state)?
            .probe(&request.source, request.privacy_masks, request.camera_id)
            .await?,
    ))
}

pub async fn devices(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<sw_protocol::CaptureDevice>>, ApiError> {
    principal.owner()?;
    Ok(Json(registry(&state)?.devices().await?))
}

pub async fn discover_onvif(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<OnvifDevice>>, ApiError> {
    principal.owner()?;
    Ok(Json(registry(&state)?.discover_onvif().await?))
}

pub async fn onvif_profiles(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    body: Result<Json<OnvifProfilesRequest>, JsonRejection>,
) -> Result<Json<Vec<OnvifProfile>>, ApiError> {
    principal.owner()?;
    Ok(Json(registry(&state)?.onvif_profiles(json(body)?).await?))
}

pub async fn onvif_stream(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    body: Result<Json<OnvifStreamRequest>, JsonRejection>,
) -> Result<Json<OnvifStream>, ApiError> {
    principal.owner()?;
    Ok(Json(registry(&state)?.onvif_stream(json(body)?).await?))
}

pub async fn upload(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
    body: Body,
) -> Result<impl IntoResponse, ApiError> {
    principal.owner()?;
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        != Some("application/octet-stream")
    {
        return Err(ApiError::unsupported_upload());
    }
    let bytes: u64 = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| ApiError::invalid("Upload requires a Content-Length"))?;
    if bytes > MAX_REPLAY_UPLOAD_BYTES {
        return Err(ApiError::upload_too_large());
    }
    let uploaded = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        registry(&state)?.upload_replay(body.into_data_stream(), bytes),
    )
    .await
    .map_err(|_| ApiError::upload_timeout())??;
    Ok((StatusCode::CREATED, Json(uploaded)))
}

pub async fn analysis_status(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Result<Json<sw_protocol::AnalysisStatus>, ApiError> {
    let id = resource_id(&id)?;
    principal.camera(id)?;
    let camera = state.store.camera(id).await?;
    let analysis = analysis(&state, camera.id, camera.epoch, camera.config.enabled);
    Ok(Json(analysis))
}

fn analysis(state: &Api, id: Uuid, epoch: u64, enabled: bool) -> sw_protocol::AnalysisStatus {
    match &state.vision {
        Some(vision) if enabled => vision.status(id, epoch),
        _ => sw_protocol::AnalysisStatus::with_state(sw_protocol::AnalysisState::Disabled),
    }
}

fn view(state: &Api, registry: &CameraRegistry, record: CameraRecord) -> Camera {
    let mut camera = registry.view(record);
    if state.vision.is_some() {
        camera.runtime.analysis = Some(analysis(
            state,
            camera.id,
            camera.source_epoch.0,
            camera.config.enabled,
        ));
    }
    camera
}

async fn mutate(
    state: &Api,
    principal: &Principal,
    headers: HeaderMap,
    action: CameraAction,
) -> Result<sw_storage::CameraResult, ApiError> {
    principal.owner()?;
    let key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<IdempotencyKey>().ok())
        .ok_or_else(|| ApiError::invalid("A valid Idempotency-Key is required"))?;
    let command = CameraCommand {
        actor: principal.id(),
        key,
        action,
    };
    let now = (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64;
    let saved = registry(state)?.apply(command, now).await?;
    if let Some(media) = &state.media
        && saved.camera.revision > 1
    {
        let camera = &saved.camera;
        media
            .retire_camera_before(camera.id, camera.epoch, camera.scene_revision)
            .await;
    }
    Ok(saved)
}

fn registry(state: &Api) -> Result<&Arc<CameraRegistry>, ApiError> {
    state.cameras.as_ref().ok_or_else(ApiError::not_found)
}

fn response(status: StatusCode, camera: Camera) -> Result<Response, ApiError> {
    let etag = HeaderValue::from_str(&format!("\"{}\"", camera.revision))
        .map_err(|_| ApiError::internal())?;
    Ok((status, [(header::ETAG, etag)], Json(camera)).into_response())
}

fn resource_id(value: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(value).map_err(|_| ApiError::invalid("Invalid camera UUID"))
}

fn expected_revision(headers: &HeaderMap) -> Result<u64, ApiError> {
    headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix('"')?.strip_suffix('"'))
        .and_then(|value| value.parse::<Counter>().ok())
        .map(|value| value.0)
        .ok_or_else(|| ApiError::invalid("A strong decimal If-Match ETag is required"))
}

fn json<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    body.map(|Json(data)| data)
        .map_err(|_| ApiError::invalid("Request body does not match the JSON contract"))
}
