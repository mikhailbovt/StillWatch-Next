use axum::{
    Extension, Json,
    extract::{
        Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{HeaderMap, HeaderValue, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use sw_domain::OperatorAction;
use sw_protocol::{
    AcknowledgeRequest, Counter, DeviceScope, FeedbackRequest, IdempotencyKey, IncidentPage,
    ResolveRequest, SilenceRequest,
};
use sw_storage::{HistoryCursor, OperatorCommand};
use uuid::Uuid;

use crate::{
    Api,
    auth::{Principal, now_ms},
    errors::ApiError,
    wire,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pagination {
    limit: Option<usize>,
    after: Option<String>,
}

pub async fn list(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    query: Result<Query<Pagination>, QueryRejection>,
) -> Result<Json<IncidentPage>, ApiError> {
    let Query(query) = query.map_err(|_| ApiError::invalid("Invalid history query"))?;
    let after = query
        .after
        .as_deref()
        .map(parse_history_cursor)
        .transpose()?;
    let page = state
        .store
        .history_visible(
            after,
            query.limit.unwrap_or(100),
            principal.allowed_cameras(),
        )
        .await?;
    let incidents = page
        .incidents
        .into_iter()
        .map(wire::incident)
        .collect::<Result<Vec<_>, _>>()?;
    let next = page
        .next
        .map(|cursor| format!("{}:{}", cursor.created_at, cursor.id));
    Ok(Json(IncidentPage { incidents, next }))
}

pub async fn detail(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    incident_response(authorized_incident(&state, &principal, &id).await?)
}

pub async fn evidence_image(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    Path((incident, packet, ordinal)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    principal.scope(DeviceScope::EvidenceRead)?;
    let item = authorized_incident(&state, &principal, &incident).await?;
    let ordinal = ordinal
        .parse::<u8>()
        .ok()
        .filter(|value| *value < 16)
        .ok_or_else(|| ApiError::invalid("Invalid evidence image ordinal"))?;
    let bytes = state
        .store
        .read_evidence(item.id, parse_uuid(&packet)?, ordinal)
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (header::CACHE_CONTROL, "private, no-store"),
        ],
        bytes,
    )
        .into_response())
}

pub async fn evidence_packet(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    Path((incident, packet)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    principal.scope(DeviceScope::EvidenceRead)?;
    let item = authorized_incident(&state, &principal, &incident).await?;
    let metadata = state
        .store
        .evidence_packet(item.id, parse_uuid(&packet)?)
        .await?;
    Ok((
        [(header::CACHE_CONTROL, "private, no-store")],
        Json(metadata),
    )
        .into_response())
}

pub async fn acknowledge(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<AcknowledgeRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    parse_json(body)?;
    apply(state, principal, id, headers, OperatorAction::Acknowledge).await
}

pub async fn resolve(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ResolveRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let body = parse_json(body)?;
    apply(
        state,
        principal,
        id,
        headers,
        OperatorAction::Resolve {
            reason: body.reason.into(),
        },
    )
    .await
}

pub async fn feedback(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<FeedbackRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let body = parse_json(body)?;
    let classification = match body.classification {
        sw_protocol::FeedbackClassification::Confirmed => {
            sw_domain::FeedbackClassification::Confirmed
        }
        sw_protocol::FeedbackClassification::FalseAlarm => {
            sw_domain::FeedbackClassification::FalseAlarm
        }
    };
    let action = OperatorAction::Feedback {
        classification,
        evidence_version: body.evidence_version.0,
        reason: body.reason.into(),
    };
    apply(state, principal, id, headers, action).await
}

pub async fn silence(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<SilenceRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let body = parse_json(body)?;
    apply(
        state,
        principal,
        id,
        headers,
        OperatorAction::Silence {
            duration_seconds: body.duration_seconds,
        },
    )
    .await
}

async fn apply(
    state: Api,
    principal: Principal,
    id: String,
    headers: HeaderMap,
    action: OperatorAction,
) -> Result<Response, ApiError> {
    principal.scope(DeviceScope::EventsAck)?;
    let incident = authorized_incident(&state, &principal, &id).await?;
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<IdempotencyKey>().ok())
        .ok_or_else(|| ApiError::invalid("A valid Idempotency-Key is required"))?;
    let revision = headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix('"')?.strip_suffix('"'))
        .and_then(|value| value.parse::<Counter>().ok())
        .ok_or_else(|| ApiError::invalid("A strong decimal If-Match ETag is required"))?;
    let command = OperatorCommand {
        incident_id: incident.id,
        actor_id: principal.id(),
        idempotency_key: idempotency_key.as_str().to_owned(),
        expected_revision: revision.0,
        action,
    };
    incident_response(state.store.apply(command, now_ms()).await?.incident)
}

async fn authorized_incident(
    state: &Api,
    principal: &Principal,
    id: &str,
) -> Result<sw_domain::Incident, ApiError> {
    let incident = state.store.get(parse_uuid(id)?).await?;
    principal.camera(incident.scope.camera_id)?;
    Ok(incident)
}
fn incident_response(item: sw_domain::Incident) -> Result<Response, ApiError> {
    let etag = HeaderValue::from_str(&format!("\"{}\"", item.revision))
        .map_err(|_| ApiError::internal())?;
    Ok(([(header::ETAG, etag)], Json(wire::incident(item)?)).into_response())
}

fn parse_uuid(value: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(value).map_err(|_| ApiError::invalid("Invalid resource UUID"))
}

fn parse_history_cursor(value: &str) -> Result<HistoryCursor, ApiError> {
    let invalid = || ApiError::invalid("Invalid history cursor");
    let (created_at, id) = value.split_once(':').ok_or_else(invalid)?;
    Ok(HistoryCursor {
        created_at: created_at.parse().map_err(|_| invalid())?,
        id: parse_uuid(id)?,
    })
}

fn parse_json<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    body.map(|Json(data)| data)
        .map_err(|_| ApiError::invalid("Request body does not match the JSON contract"))
}
