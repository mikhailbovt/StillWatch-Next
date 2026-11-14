#![forbid(unsafe_code)]

mod alerts;
mod auth;
mod cameras;
mod errors;
mod health;
mod incidents;
mod media;
mod stream;
mod trust;
mod wire;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    middleware,
    routing::{get, post},
};
use sw_engine::{CameraRegistry, MediaService, SemanticSupervisor, VisionScheduler};
use sw_protocol::{
    AnalysisState, ApiVersion, Bootstrap, CoreStatus, Health, HealthState, InferenceStatus,
    PairingProfile, SemanticStatus,
};
use sw_storage::Store;
use tokio::sync::{Semaphore, watch};

use crate::errors::ApiError;

#[derive(Clone)]
struct Api {
    store: Store,
    auth: Arc<auth::ListenerAuth>,
    shutdown: watch::Receiver<bool>,
    stream_slots: Arc<Semaphore>,
    cameras: Option<Arc<CameraRegistry>>,
    vision: Option<Arc<VisionScheduler>>,
    semantic: Option<Arc<SemanticSupervisor>>,
    media: Option<Arc<MediaService>>,
    endpoints: PairingEndpoints,
    profile: Option<PairingProfile>,
    enrollment_slots: Arc<auth::EnrollmentLimit>,
}

/// Builds the first, loopback-only trust profile. LAN binding has its own TLS and device checks.
pub fn loopback_router(
    store: Store,
    listen: SocketAddr,
    token_digest: [u8; 32],
    shutdown: watch::Receiver<bool>,
    cameras: Option<Arc<CameraRegistry>>,
    vision: Option<Arc<VisionScheduler>>,
    semantic: Option<Arc<SemanticSupervisor>>,
) -> Option<Router> {
    local_router(
        store,
        listen,
        token_digest,
        shutdown,
        ApiServices {
            cameras,
            vision,
            semantic,
            media: None,
        },
        PairingEndpoints::default(),
    )
}

#[derive(Clone)]
pub struct ApiServices {
    pub cameras: Option<Arc<CameraRegistry>>,
    pub vision: Option<Arc<VisionScheduler>>,
    pub semantic: Option<Arc<SemanticSupervisor>>,
    pub media: Option<Arc<MediaService>>,
}

pub fn local_router(
    store: Store,
    listen: SocketAddr,
    token_digest: [u8; 32],
    shutdown: watch::Receiver<bool>,
    services: ApiServices,
    endpoints: PairingEndpoints,
) -> Option<Router> {
    if !listen.ip().is_loopback() {
        return None;
    }
    let state = Api {
        store,
        auth: Arc::new(auth::ListenerAuth::local(listen, token_digest)),
        shutdown,
        stream_slots: Arc::new(Semaphore::new(32)),
        cameras: services.cameras,
        vision: services.vision,
        semantic: services.semantic,
        media: services.media,
        endpoints,
        profile: None,
        enrollment_slots: Arc::new(auth::EnrollmentLimit::default()),
    };
    Some(router(state))
}

fn router(state: Api) -> Router {
    let mut protected = Router::new()
        .route("/health/ready", get(readiness))
        .route("/api/v2/status", get(core_status))
        .route("/api/v2/semantic-status", get(semantic_status))
        .route("/api/v2/health-events", get(health::list))
        .route("/api/v2/incidents", get(incidents::list))
        .route("/api/v2/incidents/{id}", get(incidents::detail))
        .route(
            "/api/v2/incidents/{id}/acknowledgements",
            post(incidents::acknowledge),
        )
        .route("/api/v2/incidents/{id}/feedback", post(incidents::feedback))
        .route(
            "/api/v2/incidents/{id}/resolution",
            post(incidents::resolve),
        )
        .route(
            "/api/v2/incidents/{id}/evidence/{packet}",
            get(incidents::evidence_packet),
        )
        .route(
            "/api/v2/incidents/{id}/evidence/{packet}/{ordinal}",
            get(incidents::evidence_image),
        )
        .route("/api/v2/alerts/{id}/silence", post(incidents::silence))
        .route("/api/v2/alerts", get(alerts::list))
        .route("/api/v2/alerts/{alert}/seen", post(alerts::seen))
        .route("/api/v2/snapshot", get(stream::snapshot))
        .route("/api/v2/changes", get(stream::changes));
    if state.cameras.is_some() {
        protected = protected
            .route("/api/v2/cameras", get(cameras::list).post(cameras::create))
            .route(
                "/api/v2/cameras/{id}",
                get(cameras::detail)
                    .patch(cameras::patch)
                    .delete(cameras::remove),
            )
            .route("/api/v2/source-probes", post(cameras::probe))
            .route("/api/v2/capture-devices", get(cameras::devices))
            .route("/api/v2/onvif/discovery", post(cameras::discover_onvif))
            .route("/api/v2/onvif/profiles", post(cameras::onvif_profiles))
            .route("/api/v2/onvif/stream", post(cameras::onvif_stream))
            .route(
                "/api/v2/cameras/{id}/analysis-status",
                get(cameras::analysis_status),
            );
        if state.profile != Some(PairingProfile::Proxy) {
            protected = protected.route("/api/v2/replay-uploads", post(cameras::upload));
        }
    }
    if state.endpoints.get(state.profile).is_some() {
        protected = protected
            .route("/api/v2/pairing", post(trust::issue))
            .route("/api/v2/devices", get(trust::list))
            .route("/api/v2/devices/{id}", axum::routing::delete(trust::revoke));
    }
    if state.media.is_some() {
        protected = protected
            .route("/api/v2/stream-sessions", post(media::issue))
            .route(
                "/api/v2/stream-sessions/{id}/whep",
                post(media::offer)
                    .patch(media::trickle)
                    .delete(media::close)
                    .layer(DefaultBodyLimit::max(48 * 1024)),
            )
            .route("/api/v2/cameras/{id}/snapshot.jpg", get(media::snapshot));
    }
    let protected = protected.route_layer(middleware::from_fn_with_state(
        state.clone(),
        auth::require_token,
    ));
    let mut public = Router::new()
        .route("/health/live", get(liveness))
        .route("/bootstrap/info", get(bootstrap));
    if state.auth.network() {
        public = public
            .route("/api/v2/pairing/finish", post(trust::finish))
            .route("/api/v2/device-session/refresh", post(trust::refresh));
    }
    public
        .merge(protected)
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), auth::origin))
        .with_state(state)
}

pub struct EndpointIdentity {
    pub endpoint: String,
    pub certificate_der: Vec<u8>,
    pub certificate_sha256: String,
}

#[derive(Clone, Default)]
pub struct PairingEndpoints {
    pub lan: Option<Arc<EndpointIdentity>>,
    pub proxy: Option<Arc<EndpointIdentity>>,
}

impl PairingEndpoints {
    fn get(&self, profile: Option<PairingProfile>) -> Option<&EndpointIdentity> {
        match profile {
            Some(PairingProfile::Lan) => self.lan.as_deref(),
            Some(PairingProfile::Proxy) => self.proxy.as_deref(),
            None => self.lan.as_deref().or(self.proxy.as_deref()),
        }
    }
}

pub fn network_router(
    store: Store,
    host: String,
    endpoints: PairingEndpoints,
    profile: PairingProfile,
    shutdown: watch::Receiver<bool>,
    services: ApiServices,
) -> Router {
    let state = Api {
        store,
        auth: Arc::new(auth::ListenerAuth::network_listener(host)),
        shutdown,
        stream_slots: Arc::new(Semaphore::new(32)),
        cameras: services.cameras,
        vision: services.vision,
        semantic: services.semantic,
        media: services.media,
        endpoints,
        profile: Some(profile),
        enrollment_slots: Arc::new(auth::EnrollmentLimit::default()),
    };
    router(state)
}

async fn liveness() -> Json<Health> {
    Json(Health {
        state: HealthState::Live,
        api: ApiVersion::CURRENT,
    })
}

async fn readiness(State(state): State<Api>) -> (StatusCode, Json<Health>) {
    let ready = !*state.shutdown.borrow()
        && matches!(
            tokio::time::timeout(Duration::from_secs(2), state.store.healthy()).await,
            Ok(Ok(()))
        );
    let (status, state) = if ready {
        (StatusCode::OK, HealthState::Ready)
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, HealthState::Degraded)
    };
    (
        status,
        Json(Health {
            state,
            api: ApiVersion::CURRENT,
        }),
    )
}

async fn bootstrap(State(state): State<Api>) -> Json<Bootstrap> {
    Json(Bootstrap {
        instance_id: state.store.identity().instance_id,
        version: ApiVersion::CURRENT,
    })
}

async fn core_status(State(state): State<Api>) -> Json<CoreStatus> {
    Json(CoreStatus {
        version: env!("CARGO_PKG_VERSION").into(),
        inference: state.vision.as_ref().map_or_else(
            || InferenceStatus {
                state: AnalysisState::Disabled,
                backend: None,
                ready_workers: 0,
                configured_workers: 0,
                failure: None,
            },
            |vision| vision.inference_status(),
        ),
        media: state.media.as_ref().map(|media| media.status()),
    })
}

async fn semantic_status(State(state): State<Api>) -> Json<SemanticStatus> {
    Json(
        state
            .semantic
            .as_ref()
            .map_or_else(SemanticSupervisor::disabled, |worker| worker.status()),
    )
}

async fn not_found() -> ApiError {
    ApiError::not_found()
}

async fn method_not_allowed() -> ApiError {
    ApiError::method_not_allowed()
}

#[cfg(test)]
mod tests;
