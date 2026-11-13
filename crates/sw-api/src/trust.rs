use std::net::SocketAddr;

use axum::{
    Extension, Json,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::StatusCode,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sw_protocol::{
    ApiVersion, Device, DeviceCredentials, PairingFinish, PairingOffer, PairingProfile,
    PairingRequest, RefreshRequest,
};
use uuid::Uuid;

use crate::{
    Api,
    auth::{Principal, now_ms},
    errors::ApiError,
};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingQuery {
    profile: Option<PairingProfile>,
}

pub async fn issue(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    Query(query): Query<PairingQuery>,
    request: Result<Json<PairingRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<PairingOffer>), ApiError> {
    principal.owner()?;
    let identity = state
        .endpoints
        .get(query.profile.or(state.profile))
        .ok_or_else(ApiError::not_found)?;
    let Json(request) = request.map_err(|_| ApiError::invalid("Invalid pairing permissions"))?;
    let grant = state
        .store
        .issue_pairing(principal.id(), request, now_ms())
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(PairingOffer {
            api: ApiVersion::CURRENT,
            endpoint: identity.endpoint.clone(),
            core_instance_id: state.store.identity().instance_id,
            tls_certificate_sha256: identity.certificate_sha256.clone(),
            tls_certificate_der: URL_SAFE_NO_PAD.encode(&identity.certificate_der),
            challenge: grant.challenge,
            secret: grant.secret,
            expires_at: grant.expires_at,
        }),
    ))
}

pub async fn finish(
    State(state): State<Api>,
    Extension(peer): Extension<SocketAddr>,
    request: Result<Json<PairingFinish>, JsonRejection>,
) -> Result<(StatusCode, Json<DeviceCredentials>), ApiError> {
    if !state.enrollment_slots.allow(peer.ip()) {
        return Err(ApiError::rate_limited("Pairing rate limit reached"));
    }
    let Json(request) = request.map_err(|_| ApiError::invalid("Invalid pairing proof"))?;
    Ok((
        StatusCode::CREATED,
        Json(state.store.finish_pairing(request, now_ms()).await?),
    ))
}

pub async fn refresh(
    State(state): State<Api>,
    Extension(peer): Extension<SocketAddr>,
    request: Result<Json<RefreshRequest>, JsonRejection>,
) -> Result<Json<DeviceCredentials>, ApiError> {
    if !state.enrollment_slots.allow(peer.ip()) {
        return Err(ApiError::rate_limited("Session rate limit reached"));
    }
    let Json(request) = request.map_err(|_| ApiError::invalid("Invalid refresh proof"))?;
    Ok(Json(state.store.refresh_device(request, now_ms()).await?))
}

pub async fn list(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Vec<Device>>, ApiError> {
    principal.owner()?;
    Ok(Json(state.store.devices().await?))
}

pub async fn revoke(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    Path(device): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    principal.owner()?;
    state
        .store
        .revoke_device(principal.id(), device, now_ms())
        .await?;
    if let Some(media) = state.media.as_ref() {
        media.revoke(device).await;
    }
    Ok(StatusCode::NO_CONTENT)
}
