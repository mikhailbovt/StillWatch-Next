use axum::{
    Json,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use sw_protocol::{Problem, ProblemCode};
use sw_storage::StoreError;
use uuid::Uuid;

pub(crate) struct ApiError {
    status: StatusCode,
    pub problem: Problem,
}

impl ApiError {
    fn new(
        status: StatusCode,
        code: ProblemCode,
        title: &'static str,
        detail: Option<String>,
    ) -> Self {
        Self {
            status,
            problem: Problem {
                type_uri: "about:blank".into(),
                title: title.into(),
                status: status.as_u16(),
                code,
                detail,
                request_id: Some(Uuid::new_v4()),
            },
        }
    }

    pub fn invalid(detail: &'static str) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            ProblemCode::InvalidRequest,
            "Invalid request",
            Some(detail.into()),
        )
    }
    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            ProblemCode::Unauthorized,
            "Authentication required",
            None,
        )
    }
    pub fn forbidden() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            ProblemCode::Forbidden,
            "Access denied",
            None,
        )
    }
    pub fn not_found() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            ProblemCode::NotFound,
            "Resource not found",
            None,
        )
    }
    pub fn method_not_allowed() -> Self {
        Self::new(
            StatusCode::METHOD_NOT_ALLOWED,
            ProblemCode::InvalidRequest,
            "Method not allowed",
            None,
        )
    }
    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ProblemCode::InternalError,
            "Internal error",
            None,
        )
    }
    pub fn stream_limit() -> Self {
        Self::rate_limited("Change stream capacity reached")
    }
    pub fn rate_limited(title: &'static str) -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            ProblemCode::RateLimited,
            title,
            None,
        )
    }

    pub fn upload_too_large() -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            ProblemCode::CapacityExceeded,
            "Replay file exceeds 128 MiB",
            None,
        )
    }

    pub fn unsupported_upload() -> Self {
        Self::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            ProblemCode::InvalidRequest,
            "Replay upload requires application/octet-stream",
            None,
        )
    }

    pub fn upload_timeout() -> Self {
        Self::new(
            StatusCode::REQUEST_TIMEOUT,
            ProblemCode::InvalidRequest,
            "Replay upload timed out",
            None,
        )
    }
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::NotFound => Self::not_found(),
            StoreError::Revision { current } => Self::new(
                StatusCode::PRECONDITION_FAILED,
                ProblemCode::RevisionConflict,
                "Incident revision changed",
                Some(format!("Current revision is {current}")),
            ),
            StoreError::Idempotency | StoreError::IncidentConflict => Self::new(
                StatusCode::CONFLICT,
                ProblemCode::IdempotencyConflict,
                "Request conflicts with a committed operation",
                None,
            ),
            StoreError::CursorExpired => Self::new(
                StatusCode::CONFLICT,
                ProblemCode::CursorExpired,
                "Change cursor expired",
                Some("Fetch a fresh snapshot before reconnecting".into()),
            ),
            StoreError::CameraCapacity => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                ProblemCode::CapacityExceeded,
                "Capacity exceeded",
                None,
            ),
            StoreError::Invalid => Self::invalid("The storage request is invalid"),
            StoreError::Credential => Self::unauthorized(),
            StoreError::DeviceCapacity => Self::rate_limited("Device or pairing capacity reached"),
            StoreError::DatabaseCapacity => Self::new(
                StatusCode::INSUFFICIENT_STORAGE,
                ProblemCode::StorageUnavailable,
                "Core database storage capacity reached",
                None,
            ),
            StoreError::Domain(error) => Self::new(
                StatusCode::CONFLICT,
                ProblemCode::InvalidTransition,
                "Invalid incident transition",
                Some(error.to_string()),
            ),
            _ => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                ProblemCode::StorageUnavailable,
                "Storage unavailable",
                None,
            ),
        }
    }
}

impl From<sw_engine::RegistryError> for ApiError {
    fn from(error: sw_engine::RegistryError) -> Self {
        use sw_engine::RegistryError;
        match error {
            RegistryError::Store(error) => error.into(),
            RegistryError::Invalid
            | RegistryError::Source
            | RegistryError::Worker(sw_engine::WorkerError::Source) => Self::invalid(
                "Source is invalid, unavailable, or outside the configured capture scope",
            ),
            RegistryError::Busy => Self::new(
                StatusCode::TOO_MANY_REQUESTS,
                ProblemCode::CapacityExceeded,
                "Capture resources busy",
                None,
            ),
            RegistryError::UploadQuota => Self::new(
                StatusCode::INSUFFICIENT_STORAGE,
                ProblemCode::CapacityExceeded,
                "Replay storage budget reached",
                None,
            ),
            RegistryError::UploadStorage(_) => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                ProblemCode::StorageUnavailable,
                "Replay storage unavailable",
                None,
            ),
            RegistryError::Onvif(error) => {
                use sw_engine::OnvifError;
                match error {
                    OnvifError::Invalid => {
                        Self::invalid("ONVIF endpoint or request is outside the configured scope")
                    }
                    OnvifError::Authentication => {
                        Self::invalid("ONVIF camera authentication failed")
                    }
                    OnvifError::Busy => Self::rate_limited("ONVIF resources busy"),
                    OnvifError::DiscoveryDisabled => Self::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        ProblemCode::ProviderUnavailable,
                        "ONVIF multicast is not configured on this Core",
                        None,
                    ),
                    _ => Self::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        ProblemCode::ProviderUnavailable,
                        "ONVIF camera is unavailable or returned an unsupported response",
                        None,
                    ),
                }
            }
            RegistryError::Worker(sw_engine::WorkerError::NetworkPolicyUnavailable) => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                ProblemCode::ProviderUnavailable,
                "RTSP capture requires a peer-confined decoder",
                None,
            ),
            RegistryError::Worker(_) | RegistryError::WorkerUnavailable => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                ProblemCode::ProviderUnavailable,
                "Capture worker unavailable",
                None,
            ),
        }
    }
}

impl From<sw_engine::MediaError> for ApiError {
    fn from(error: sw_engine::MediaError) -> Self {
        use sw_engine::MediaError;
        match error {
            MediaError::Missing => Self::not_found(),
            MediaError::Capacity => Self::rate_limited("Stream capacity reached"),
            MediaError::Invalid | MediaError::Offer => {
                Self::invalid("Invalid or unsupported WHEP offer")
            }
            MediaError::Conflict => Self::new(
                StatusCode::CONFLICT,
                ProblemCode::InvalidTransition,
                "Stream session state changed",
                None,
            ),
            MediaError::Unavailable | MediaError::Io(_) => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                ProblemCode::ProviderUnavailable,
                "Media unavailable",
                None,
            ),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let id = self.problem.request_id;
        let mut response = (self.status, Json(self.problem)).into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        if self.status == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        if let Some(value) = id.and_then(|id| HeaderValue::from_str(&id.to_string()).ok()) {
            response.headers_mut().insert("x-request-id", value);
        }
        response
    }
}
