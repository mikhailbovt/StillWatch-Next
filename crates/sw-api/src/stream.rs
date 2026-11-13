use std::{convert::Infallible, time::Duration};

use axum::{
    Extension, Json,
    extract::{Query, State, rejection::QueryRejection},
    http::HeaderMap,
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
};
use serde::Deserialize;
use sw_protocol::{Change, ChangeKind, Cursor, EventSchema, Snapshot};
use sw_storage::Change as StoreChange;

use crate::{Api, auth::Principal, errors::ApiError, wire};

const BATCH: usize = 100;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resume {
    cursor: Option<String>,
}

pub async fn snapshot(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Snapshot>, ApiError> {
    let snapshot = state
        .store
        .snapshot_visible(principal.allowed_cameras())
        .await?;
    let incidents = snapshot
        .incidents
        .into_iter()
        .map(wire::incident)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(Snapshot {
        cursor: wire::cursor(snapshot.cursor),
        incidents,
    }))
}

pub async fn changes(
    State(state): State<Api>,
    Extension(principal): Extension<Principal>,
    headers: HeaderMap,
    query: Result<Query<Resume>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(query) = query.map_err(|_| ApiError::invalid("Invalid changes query"))?;
    let parse = |cursor: &str| {
        cursor
            .parse::<Cursor>()
            .map_err(|_| ApiError::invalid("Invalid change cursor"))
    };
    let header = headers
        .get("last-event-id")
        .map(|value| {
            parse(
                value
                    .to_str()
                    .map_err(|_| ApiError::invalid("Invalid Last-Event-ID"))?,
            )
        })
        .transpose()?;
    let query = query.cursor.as_deref().map(parse).transpose()?;
    if header.is_some() && query.is_some() && header != query {
        return Err(ApiError::invalid("Resume cursors disagree"));
    }
    let mut cursor = match header.or(query) {
        Some(cursor) => wire::store_cursor(cursor),
        None => {
            state
                .store
                .snapshot_visible(principal.allowed_cameras())
                .await?
                .cursor
        }
    };
    let permit = state
        .stream_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::stream_limit())?;
    let mut wake = state.store.subscribe();
    wake.borrow_and_update();
    let mut pending = state.store.changes(cursor, BATCH).await?;
    let mut shutdown = state.shutdown.clone();
    let events = async_stream::stream! {
        let _permit = permit;
        let close_at = tokio::time::Instant::now() + Duration::from_secs(30 * 60);
        loop {
            if *shutdown.borrow() || tokio::time::Instant::now() >= close_at { break; }
            if !principal.check_session(&state.store).await { break; }
            for change in pending.drain(..) {
                cursor = change.cursor;
                if principal.camera(change.incident.scope.camera_id).is_err() {
                    continue;
                }
                match event(change) {
                    Ok(event) => yield Ok::<_, Infallible>(event),
                    Err(error) => { yield Ok(error_event(error)); return; }
                }
            }
            wake.borrow_and_update();
            match state.store.changes(cursor, BATCH).await {
                Ok(changes) if !changes.is_empty() => pending = changes,
                Ok(_) => {
                    tokio::select! {
                        result = wake.changed() => if result.is_err() { break; },
                        _ = shutdown.changed() => break,
                        _ = tokio::time::sleep_until(close_at) => break,
                        _ = tokio::time::sleep(Duration::from_secs(5)) => {},
                    }
                }
                Err(error) => { yield Ok(error_event(error.into())); break; }
            }
        }
    };
    Ok(Sse::new(events)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response())
}

fn event(change: StoreChange) -> Result<Event, ApiError> {
    let item = wire::incident(change.incident)?;
    let cursor = wire::cursor(change.cursor);
    let (name, kind) = if change.opened {
        ("incident.opened", ChangeKind::IncidentOpened(item.clone()))
    } else {
        (
            "incident.updated",
            ChangeKind::IncidentUpdated(item.clone()),
        )
    };
    Event::default()
        .id(cursor.to_string())
        .event(name)
        .json_data(Change {
            schema_version: EventSchema::V1,
            stream_generation: cursor.generation,
            sequence: cursor.sequence,
            entity_id: item.id,
            entity_revision: item.revision,
            occurred_at: item.updated_at,
            kind,
        })
        .map_err(|_| ApiError::internal())
}

fn error_event(error: ApiError) -> Event {
    Event::default()
        .event("resync")
        .json_data(error.problem)
        .unwrap_or_else(|_| Event::default().event("resync").data("{}"))
}
