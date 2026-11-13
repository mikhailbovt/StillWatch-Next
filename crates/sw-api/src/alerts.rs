use axum::{
    Extension, Json,
    extract::{Path, State, rejection::JsonRejection},
    http::StatusCode,
};
use sw_protocol::{AlertFeed, AlertReceipt, Counter};

use crate::{
    Api,
    auth::{Principal, now_ms},
    errors::ApiError,
};

pub async fn list(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<AlertFeed>, ApiError> {
    Ok(Json(
        state
            .store
            .active_alerts(principal.id(), principal.allowed_cameras(), now_ms())
            .await?,
    ))
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
