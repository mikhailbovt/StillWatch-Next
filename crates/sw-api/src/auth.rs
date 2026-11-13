use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::Mutex,
    time::{Duration, Instant},
};

use axum::{
    extract::{Request, State},
    http::{HeaderValue, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use sw_protocol::DeviceScope;
use sw_storage::DeviceAccess;
use uuid::Uuid;

use crate::{Api, errors::ApiError};

pub struct ListenerAuth {
    host: String,
    localhost: Option<String>,
    digest: Option<[u8; 32]>,
}

impl ListenerAuth {
    pub fn local(listen: SocketAddr, digest: [u8; 32]) -> Self {
        Self {
            host: listen.to_string(),
            localhost: Some(format!("localhost:{}", listen.port())),
            digest: Some(digest),
        }
    }

    pub fn network_listener(host: String) -> Self {
        Self {
            host,
            localhost: None,
            digest: None,
        }
    }

    pub fn network(&self) -> bool {
        self.digest.is_none()
    }

    fn host_allowed(&self, host: &str) -> bool {
        host.eq_ignore_ascii_case(&self.host)
            || self
                .localhost
                .as_ref()
                .is_some_and(|name| host.eq_ignore_ascii_case(name))
    }
}

#[derive(Clone)]
pub enum Principal {
    Local(Uuid),
    Device { access: DeviceAccess, token: String },
}

impl Principal {
    pub fn id(&self) -> Uuid {
        match self {
            Self::Local(id) => *id,
            Self::Device { access, .. } => access.device.id,
        }
    }

    pub fn scope(&self, scope: DeviceScope) -> Result<(), ApiError> {
        match self {
            Self::Local(_) => Ok(()),
            Self::Device { access, .. } if access.allows(scope) => Ok(()),
            _ => Err(ApiError::forbidden()),
        }
    }

    pub fn owner(&self) -> Result<(), ApiError> {
        match self {
            Self::Local(_) => Ok(()),
            Self::Device { access, .. } if access.owner() => Ok(()),
            _ => Err(ApiError::forbidden()),
        }
    }

    pub fn camera(&self, id: Uuid) -> Result<(), ApiError> {
        match self {
            Self::Local(_) => Ok(()),
            Self::Device { access, .. } if access.camera(id) => Ok(()),
            _ => Err(ApiError::not_found()),
        }
    }

    pub fn allowed_cameras(&self) -> Option<Vec<Uuid>> {
        match self {
            Self::Local(_) => None,
            Self::Device { access, .. } if access.owner() => None,
            Self::Device { access, .. } => Some(access.device.camera_ids.clone()),
        }
    }

    pub async fn check_session(&self, store: &sw_storage::Store) -> bool {
        match self {
            Self::Local(_) => true,
            Self::Device { access, token } => {
                let now = now_ms();
                now < access.expires_at && store.device_access(token, now).await.is_ok()
            }
        }
    }
}

#[derive(Default)]
pub struct EnrollmentLimit(Mutex<HashMap<IpAddr, (Instant, u16)>>);

impl EnrollmentLimit {
    pub fn allow(&self, peer: IpAddr) -> bool {
        let Ok(mut visits) = self.0.lock() else {
            return false;
        };
        let now = Instant::now();
        visits.retain(|_, (created, _)| now.duration_since(*created) < Duration::from_secs(60));
        if visits.len() >= 512 && !visits.contains_key(&peer) {
            return false;
        }
        let (_, count) = visits.entry(peer).or_insert((now, 0));
        *count = count.saturating_add(1);
        *count <= 24
    }
}

pub async fn origin(State(state): State<Api>, request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    if !host.is_some_and(|host| state.auth.host_allowed(host)) {
        return ApiError::invalid("Host must match the configured listener").into_response();
    }
    if let Some(origin) = request.headers().get(header::ORIGIN) {
        let scheme = if state.auth.network() {
            "https://"
        } else {
            "http://"
        };
        let matching = origin
            .to_str()
            .ok()
            .and_then(|origin| origin.strip_prefix(scheme))
            == host;
        if !matching {
            return ApiError::forbidden().into_response();
        }
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

pub async fn require_token(State(state): State<Api>, mut request: Request, next: Next) -> Response {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, token)| {
            scheme.eq_ignore_ascii_case("bearer")
                && token.len() == 64
                && token.bytes().all(|b| b.is_ascii_hexdigit())
        })
        .map(|(_, token)| token.to_owned());
    let Some(token) = token else {
        return ApiError::unauthorized().into_response();
    };
    let principal = if let Some(expected) = state.auth.digest {
        let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        if !bool::from(digest.ct_eq(&expected)) {
            return ApiError::unauthorized().into_response();
        }
        Principal::Local(state.store.identity().instance_id)
    } else {
        match state.store.device_access(&token, now_ms()).await {
            Ok(access) => Principal::Device { access, token },
            Err(error) => return ApiError::from(error).into_response(),
        }
    };
    request.extensions_mut().insert(principal);
    next.run(request).await
}

pub fn now_ms() -> i64 {
    (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}
