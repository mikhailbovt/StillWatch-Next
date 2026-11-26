use axum::{
    Extension, Json,
    extract::{Query, State, rejection::QueryRejection},
};
use serde::Deserialize;
use sw_protocol::{Counter, HealthEventPage};

use crate::{Api, auth::Principal, errors::ApiError};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HealthQuery {
    before: Option<Counter>,
    limit: Option<usize>,
    #[serde(default)]
    active_only: bool,
}

pub(crate) async fn list(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    query: Result<Query<HealthQuery>, QueryRejection>,
) -> Result<Json<HealthEventPage>, ApiError> {
    let Query(query) = query.map_err(|_| ApiError::invalid("Invalid health events query"))?;
    let page = state
        .store
        .health_events(
            query.before,
            query.limit.unwrap_or(100),
            query.active_only,
            principal.allowed_cameras(),
        )
        .await?;
    Ok(Json(page))
}
