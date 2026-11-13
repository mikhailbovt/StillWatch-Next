use std::net::SocketAddr;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use sha2::{Digest, Sha256};
use sw_domain::{Assessment, Category, NewIncident, SourceScope};
use sw_protocol::{Incident, Lifecycle, Problem, ProblemCode, Snapshot};
use sw_storage::{Store, StoreConfig};
use tokio::sync::watch;
use tower::ServiceExt;
use uuid::Uuid;

use super::loopback_router;

const LOCAL: &str = "127.0.0.1:8765";
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn setup(store: Store, shutdown: watch::Receiver<bool>) -> Router {
    loopback_router(
        store,
        LOCAL.parse::<SocketAddr>().unwrap(),
        Sha256::digest(TOKEN.as_bytes()).into(),
        shutdown,
        None,
        None,
        None,
    )
    .unwrap()
}

fn request(method: &str, path: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, LOCAL)
}

async fn body<T: serde::de::DeserializeOwned>(response: axum::response::Response) -> T {
    let bytes = to_bytes(response.into_body(), 16 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn operator_commands_return_committed_revisions_and_problem_details() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path(), StoreConfig::default()).unwrap();
    let id = Uuid::new_v4();
    store
        .create(
            NewIncident {
                id,
                scope: SourceScope {
                    camera_id: Uuid::new_v4(),
                    epoch: 1,
                    scene_revision: 1,
                },
                category: Category::Fire,
                reason: "visible smoke".into(),
                analysis_origin: None,
                first_source_us: 0,
                last_source_us: 600_000,
                live: true,
                assessment: Assessment::Concern,
            },
            1_000,
        )
        .await
        .unwrap();
    let (_stop, shutdown) = watch::channel(false);
    let router = setup(store, shutdown);
    let path = format!("/api/v2/incidents/{id}/acknowledgements");
    let command = |revision: &str, key: &str| {
        request("POST", &path)
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .header(header::IF_MATCH, revision)
            .header("idempotency-key", key)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .unwrap()
    };
    let acknowledged = router
        .clone()
        .oneshot(command("\"1\"", "ack"))
        .await
        .unwrap();
    assert_eq!(acknowledged.status(), StatusCode::OK);
    assert_eq!(acknowledged.headers()[header::ETAG], "\"2\"");
    let incident: Incident = body(acknowledged).await;
    assert_eq!(incident.lifecycle, Lifecycle::Open);
    assert!(incident.acknowledgement.is_some());
    let replay = router
        .clone()
        .oneshot(command("\"1\"", "ack"))
        .await
        .unwrap();
    assert_eq!(body::<Incident>(replay).await, incident);
    let stale = router
        .clone()
        .oneshot(command("\"1\"", "new"))
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::PRECONDITION_FAILED);
    assert_eq!(
        stale.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
    assert_eq!(
        body::<Problem>(stale).await.code,
        ProblemCode::RevisionConflict
    );
    let snapshot = request("GET", "/api/v2/snapshot")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let snapshot: Snapshot = body(router.oneshot(snapshot).await.unwrap()).await;
    assert_eq!(snapshot.cursor.sequence.0, 2);
    assert_eq!(snapshot.incidents, vec![incident]);
}

#[tokio::test]
async fn loopback_host_and_origin_checks_apply_before_access() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path(), StoreConfig::default()).unwrap();
    let (_stop, shutdown) = watch::channel(false);
    let router = setup(store, shutdown);
    let anonymous = request("GET", "/api/v2/incidents")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        router.clone().oneshot(anonymous).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let remote = Request::builder()
        .uri("/bootstrap/info")
        .header(header::HOST, "example.test")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        router.clone().oneshot(remote).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    let crossed = request("GET", "/health/live")
        .header(header::ORIGIN, "https://example.test")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        router.oneshot(crossed).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
}
