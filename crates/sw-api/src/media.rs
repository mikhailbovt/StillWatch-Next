use std::sync::Arc;

use axum::{
    Extension, Json,
    body::Bytes,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use sw_engine::MediaService;
use sw_protocol::{DeviceScope, StreamGrant, StreamRequest};
use uuid::Uuid;

use crate::{
    Api,
    auth::{Principal, now_ms},
    errors::ApiError,
};

pub async fn issue(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    body: Result<Json<StreamRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<StreamGrant>), ApiError> {
    principal.scope(DeviceScope::StreamsRead)?;
    let Json(request) = body.map_err(|_| ApiError::invalid("Invalid stream request"))?;
    principal.camera(request.camera_id)?;
    Ok((
        StatusCode::CREATED,
        Json(media(&state)?.issue(principal.id(), request, now_ms())?),
    ))
}

pub async fn offer(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, ApiError> {
    principal.scope(DeviceScope::StreamsRead)?;
    require_type(&headers, "application/sdp")?;
    let media = media(&state)?;
    let answer = media.offer(principal.id(), id, body).await?;
    if !principal.check_session(&state.store).await {
        let _ = media.close(principal.id(), id).await;
        return Err(ApiError::unauthorized());
    }
    let mut response = (StatusCode::CREATED, answer.sdp).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/sdp"),
    );
    let location = format!("/api/v2/stream-sessions/{id}/whep");
    headers.insert(
        header::LOCATION,
        HeaderValue::from_str(&location).map_err(|_| ApiError::internal())?,
    );
    if let Some(value) = answer
        .etag
        .as_deref()
        .and_then(|value| HeaderValue::from_str(value).ok())
    {
        headers.insert(header::ETAG, value);
    }
    Ok(response)
}

pub async fn trickle(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    principal.scope(DeviceScope::StreamsRead)?;
    require_type(&headers, "application/trickle-ice-sdpfrag")?;
    let etag = headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::invalid("If-Match is required for WHEP PATCH"))?;
    media(&state)?
        .trickle(principal.id(), id, etag, body.to_vec())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn close(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    principal.scope(DeviceScope::StreamsRead)?;
    media(&state)?.close(principal.id(), id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn snapshot(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    principal.scope(DeviceScope::StreamsRead)?;
    principal.camera(id)?;
    let (jpeg, age) = match media(&state)?.snapshot(id).await {
        Err(sw_engine::MediaError::Capacity) => {
            return Err(ApiError::rate_limited("Snapshot encoding capacity reached"));
        }
        result => result?,
    };
    let age = HeaderValue::from_str(&age.to_string()).map_err(|_| ApiError::internal())?;
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg")),
            (HeaderName::from_static("x-frame-age-ms"), age),
        ],
        jpeg,
    )
        .into_response())
}

fn media(state: &Api) -> Result<&Arc<MediaService>, ApiError> {
    state.media.as_ref().ok_or_else(ApiError::not_found)
}

fn require_type(headers: &HeaderMap, expected: &str) -> Result<(), ApiError> {
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        != Some(expected)
    {
        return Err(ApiError::invalid("Invalid WHEP content type"));
    }
    Ok(())
}
