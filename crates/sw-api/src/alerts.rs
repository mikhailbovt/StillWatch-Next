use axum::{
    Extension, Json,
    extract::{
        Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::StatusCode,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::hmac;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sw_protocol::{
    AlertFeed, AlertReceipt, Counter, IosPushNotice, IosPushRegistration, UtcTimestamp,
};
use sw_storage::{AlertScan, MAX_ALERT_PAGE, StoreError};
use uuid::Uuid;

use crate::{
    Api,
    auth::{Principal, now_ms},
    errors::ApiError,
};

const CURSOR_LIFETIME_MS: i64 = 2 * 60_000;

pub fn cursor_key() -> hmac::Key {
    hmac::Key::generate(hmac::HMAC_SHA256, &ring::rand::SystemRandom::new())
        .expect("operating system randomness for alert cursors")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pagination {
    after: Option<String>,
    limit: Option<usize>,
}

struct Continuation {
    generation: Uuid,
    device: Uuid,
    scope: [u8; 32],
    expires: i64,
    head: Counter,
    attention: u8,
    id: Counter,
}

pub async fn list(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    query: Result<Query<Pagination>, QueryRejection>,
) -> Result<Json<AlertFeed>, ApiError> {
    let Query(query) = query.map_err(|_| ApiError::invalid("Invalid alert query"))?;
    let limit = query.limit.unwrap_or(MAX_ALERT_PAGE);
    if !(1..=MAX_ALERT_PAGE).contains(&limit) {
        return Err(ApiError::invalid("Alert page limit must be 1 to 128"));
    }
    let actor = principal.id();
    let mut cameras = principal.allowed_cameras();
    if let Some(scope) = &mut cameras {
        scope.sort_unstable();
    }
    let scope_digest = {
        let mut digest = Sha256::new();
        digest.update([u8::from(cameras.is_some())]);
        for camera in cameras.iter().flatten() {
            digest.update(camera.as_bytes());
        }
        <[u8; 32]>::from(digest.finalize())
    };
    let generation = state.store.identity().generation;
    let now = now_ms();
    let prior = query
        .after
        .as_deref()
        .map(|cursor| {
            let cursor = decode(&state.alert_cursor_key, cursor)?;
            if cursor.generation != generation
                || cursor.device != actor
                || cursor.scope != scope_digest
                || cursor.expires <= now
                || cursor.expires > now.saturating_add(CURSOR_LIFETIME_MS)
            {
                return Err(StoreError::CursorExpired);
            }
            Ok(cursor)
        })
        .transpose()
        .map_err(|_| ApiError::alert_cursor_expired())?;
    let expires = prior
        .as_ref()
        .map_or(now.saturating_add(CURSOR_LIFETIME_MS), |value| {
            value.expires
        });
    let after = prior.map(|value| AlertScan {
        head: value.head,
        attention: value.attention,
        id: value.id,
    });
    let page = state
        .store
        .active_alert_page(actor, cameras, now, after, limit)
        .await
        .map_err(|error| match error {
            StoreError::CursorExpired => ApiError::alert_cursor_expired(),
            other => other.into(),
        })?;
    let next = page.next.map(|next| {
        encode(
            &state.alert_cursor_key,
            Continuation {
                generation,
                device: actor,
                scope: scope_digest,
                expires,
                head: next.head,
                attention: next.attention,
                id: next.id,
            },
        )
    });
    Ok(Json(AlertFeed {
        alerts: page.alerts,
        next,
        observed_at: Some(UtcTimestamp::from_unix_millis(now).map_err(|_| ApiError::internal())?),
        generation: Some(generation),
        head: Some(page.head),
    }))
}

fn encode(key: &hmac::Key, cursor: Continuation) -> String {
    let mut bytes = Vec::with_capacity(121);
    bytes.extend_from_slice(cursor.generation.as_bytes());
    bytes.extend_from_slice(cursor.device.as_bytes());
    bytes.extend_from_slice(&cursor.scope);
    bytes.extend_from_slice(&cursor.expires.to_be_bytes());
    bytes.extend_from_slice(&cursor.head.0.to_be_bytes());
    bytes.push(cursor.attention);
    bytes.extend_from_slice(&cursor.id.0.to_be_bytes());
    let signature = hmac::sign(key, &bytes);
    bytes.extend_from_slice(signature.as_ref());
    URL_SAFE_NO_PAD.encode(bytes)
}

fn decode(key: &hmac::Key, value: &str) -> Result<Continuation, StoreError> {
    const PAYLOAD_LEN: usize = 89;
    const WIRE_LEN: usize = 121;
    if value.len() != (WIRE_LEN * 4).div_ceil(3) {
        return Err(StoreError::CursorExpired);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| StoreError::CursorExpired)?;
    if bytes.len() != WIRE_LEN {
        return Err(StoreError::CursorExpired);
    }
    let (payload, signature) = bytes.split_at(PAYLOAD_LEN);
    hmac::verify(key, payload, signature).map_err(|_| StoreError::CursorExpired)?;
    let invalid = |_| StoreError::CursorExpired;
    Ok(Continuation {
        generation: Uuid::from_slice(&payload[..16]).map_err(|_| StoreError::CursorExpired)?,
        device: Uuid::from_slice(&payload[16..32]).map_err(|_| StoreError::CursorExpired)?,
        scope: payload[32..64].try_into().map_err(invalid)?,
        expires: i64::from_be_bytes(payload[64..72].try_into().map_err(invalid)?),
        head: Counter(u64::from_be_bytes(
            payload[72..80].try_into().map_err(invalid)?,
        )),
        attention: payload[80],
        id: Counter(u64::from_be_bytes(
            payload[81..89].try_into().map_err(invalid)?,
        )),
    })
}

pub async fn seen(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    Path(alert): Path<String>,
    receipt: Result<Json<AlertReceipt>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let alert = alert
        .parse::<Counter>()
        .map_err(|_| ApiError::invalid("Alert ID is invalid"))?;
    let Json(receipt) = receipt.map_err(|_| ApiError::invalid("Alert receipt is invalid"))?;
    state
        .store
        .record_alert_seen(
            principal.id(),
            alert,
            receipt.attention_cycle,
            principal.allowed_cameras(),
            now_ms(),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn register_ios(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    body: Result<Json<IosPushRegistration>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let Principal::Device { access, .. } = principal else {
        return Err(ApiError::forbidden());
    };
    if !state.ios_push {
        return Err(ApiError::not_found());
    }
    if !access.owner() && access.device.camera_ids.is_empty() {
        return Err(ApiError::forbidden());
    }
    let Json(body) =
        body.map_err(|_| ApiError::invalid("Invalid iOS notification registration"))?;
    state
        .store
        .register_ios_push(access.device.id, body.token, now_ms())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn unregister_ios(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
) -> Result<StatusCode, ApiError> {
    let Principal::Device { access, .. } = principal else {
        return Err(ApiError::forbidden());
    };
    state.store.unregister_ios_push(access.device.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn ios_notice(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    Path(notice): Path<Uuid>,
) -> Result<Json<IosPushNotice>, ApiError> {
    let Principal::Device { access, .. } = principal else {
        return Err(ApiError::forbidden());
    };
    Ok(Json(
        state
            .store
            .ios_push_notice(access.device.id, notice, now_ms())
            .await?,
    ))
}
