#![forbid(unsafe_code)]

#[cfg(feature = "blocking")]
pub mod blocking;
mod pairing;
mod stream;

use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use reqwest::{Method, RequestBuilder, Response, Url, header};
use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;
use uuid::Uuid;

pub use pairing::{DeviceKey, DeviceSession, PairedEndpoint, SessionPersistence, enroll};
pub use stream::{ChangeStream, StreamMessage};
pub use sw_protocol as protocol;
use sw_protocol::{
    AcknowledgeRequest, AlertFeed, AlertReceipt, AnalysisStatus, ApiVersion, Bootstrap, Camera,
    CameraConfig, CameraPage, CameraPatch, CaptureDevice, Counter, Device, EvidencePacket,
    FeedbackRequest, Health, HealthEventPage, IdempotencyKey, Incident, IncidentPage,
    MAX_REPLAY_UPLOAD_BYTES, OnvifDevice, OnvifProfile, OnvifProfilesRequest, OnvifStream,
    OnvifStreamRequest, PairingOffer, PairingRequest, Problem, ReplayUpload, ResolveRequest,
    SemanticStatus, SilenceRequest, Snapshot, SourceProbe, SourceProbeRequest, StreamGrant,
    StreamRequest,
};

const TIMEOUT: Duration = Duration::from_secs(10);
const CAMERA_SETUP_TIMEOUT: Duration = Duration::from_secs(22);
const RESPONSE_LIMIT: usize = 4 * 1024 * 1024;

fn health_events_path(before: Option<Counter>, limit: u16, active_only: bool) -> String {
    let before = before.map_or_else(String::new, |sequence| format!("&before={sequence}"));
    format!("/api/v2/health-events?limit={limit}&active_only={active_only}{before}")
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid Core endpoint or certificate pin")]
    Endpoint,
    #[error("network or TLS connection failed")]
    Transport,
    #[error("request deadline expired")]
    Deadline,
    #[error("Core returned HTTP {status}")]
    Http {
        status: u16,
        problem: Option<Box<Problem>>,
    },
    #[error("Core response did not match the expected wire format")]
    Protocol,
    #[error("Core uses an incompatible API version or instance identity")]
    Identity,
    #[error("device credentials are unavailable")]
    Credentials,
}

impl Error {
    pub fn problem(&self) -> Option<&Problem> {
        match self {
            Self::Http { problem, .. } => problem.as_deref(),
            _ => None,
        }
    }

    fn transport(error: reqwest::Error) -> Self {
        if error.is_timeout() {
            Self::Deadline
        } else {
            Self::Transport
        }
    }
}

pub type CredentialFuture<'a> = Pin<Box<dyn Future<Output = Result<String, Error>> + Send + 'a>>;

pub trait Credentials: Send + Sync {
    fn token(&self) -> CredentialFuture<'_>;
    fn renew<'a>(&'a self, rejected: &'a str) -> CredentialFuture<'a> {
        let _ = rejected;
        self.token()
    }
}

pub struct StaticToken(String);

impl StaticToken {
    pub fn new(value: String) -> Result<Self, Error> {
        if !valid_token(&value) {
            return Err(Error::Credentials);
        }
        Ok(Self(value))
    }
}

impl Credentials for StaticToken {
    fn token(&self) -> CredentialFuture<'_> {
        Box::pin(async { Ok(self.0.clone()) })
    }
}

#[derive(Clone)]
pub struct Client {
    endpoint: Url,
    instance: Option<Uuid>,
    http: reqwest::Client,
    credentials: Option<Arc<dyn Credentials>>,
}

impl Client {
    pub fn loopback(endpoint: &str, credentials: Arc<dyn Credentials>) -> Result<Self, Error> {
        Ok(Self {
            endpoint: endpoint_url(endpoint, false)?,
            instance: None,
            // Loopback HTTP must also work in an image with no CA bundle.
            http: http_builder()
                .tls_certs_only([])
                .build()
                .map_err(Error::transport)?,
            credentials: Some(credentials),
        })
    }

    pub fn pinned(
        endpoint: &PairedEndpoint,
        credentials: Arc<dyn Credentials>,
    ) -> Result<Self, Error> {
        let mut client = Self::unauthenticated(endpoint)?;
        client.credentials = Some(credentials);
        Ok(client)
    }

    fn unauthenticated(endpoint: &PairedEndpoint) -> Result<Self, Error> {
        Ok(Self {
            endpoint: endpoint.url.clone(),
            instance: Some(endpoint.instance()),
            http: pinned_http(&endpoint.der)?,
            credentials: None,
        })
    }

    pub async fn capabilities(&self) -> Result<Bootstrap, Error> {
        let response: Bootstrap = self
            .json(Method::GET, "/bootstrap/info", None, None, false)
            .await?;
        if !response.version.supports(ApiVersion::CURRENT)
            || self.instance.is_some_and(|id| id != response.instance_id)
        {
            return Err(Error::Identity);
        }
        Ok(response)
    }

    pub async fn readiness(&self) -> Result<Health, Error> {
        self.get("/health/ready").await
    }

    pub async fn status(&self) -> Result<sw_protocol::CoreStatus, Error> {
        self.get("/api/v2/status").await
    }

    pub async fn semantic_status(&self) -> Result<SemanticStatus, Error> {
        self.get("/api/v2/semantic-status").await
    }

    pub async fn health_events(
        &self,
        before: Option<Counter>,
        limit: u16,
        active_only: bool,
    ) -> Result<HealthEventPage, Error> {
        self.get(&health_events_path(before, limit, active_only))
            .await
    }

    pub async fn cameras(&self, after: Option<Uuid>, limit: u16) -> Result<CameraPage, Error> {
        let after = after.map_or_else(String::new, |id| format!("&after={id}"));
        self.get(&format!("/api/v2/cameras?limit={limit}{after}"))
            .await
    }

    pub async fn camera(&self, id: Uuid) -> Result<Camera, Error> {
        self.get(&format!("/api/v2/cameras/{id}")).await
    }

    pub async fn analysis(&self, id: Uuid) -> Result<AnalysisStatus, Error> {
        self.get(&format!("/api/v2/cameras/{id}/analysis-status"))
            .await
    }

    pub async fn capture_devices(&self) -> Result<Vec<CaptureDevice>, Error> {
        self.get("/api/v2/capture-devices").await
    }

    pub async fn discover_onvif(&self) -> Result<Vec<OnvifDevice>, Error> {
        self.json(Method::POST, "/api/v2/onvif/discovery", None, None, true)
            .await
    }

    pub async fn onvif_profiles(
        &self,
        request: &OnvifProfilesRequest,
    ) -> Result<Vec<OnvifProfile>, Error> {
        self.json(
            Method::POST,
            "/api/v2/onvif/profiles",
            Some(body(request)?),
            None,
            true,
        )
        .await
    }

    pub async fn onvif_stream(&self, request: &OnvifStreamRequest) -> Result<OnvifStream, Error> {
        self.json(
            Method::POST,
            "/api/v2/onvif/stream",
            Some(body(request)?),
            None,
            true,
        )
        .await
    }

    pub async fn probe_source(&self, request: &SourceProbeRequest) -> Result<SourceProbe, Error> {
        self.json(
            Method::POST,
            "/api/v2/source-probes",
            Some(body(request)?),
            None,
            true,
        )
        .await
    }

    pub async fn upload_replay(&self, bytes: Vec<u8>) -> Result<ReplayUpload, Error> {
        if bytes.is_empty() || bytes.len() as u64 > MAX_REPLAY_UPLOAD_BYTES {
            return Err(Error::Protocol);
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(125);
        let result = tokio::time::timeout_at(deadline, async {
            let mut request = self
                .request(Method::POST, "/api/v2/replay-uploads")?
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .body(bytes);
            if let Some(credentials) = &self.credentials {
                let token = credentials.token().await?;
                if !valid_token(&token) {
                    return Err(Error::Credentials);
                }
                request = request.bearer_auth(token);
            }
            let response = check(request.send().await.map_err(Error::transport)?).await?;
            serde_json::from_slice(&read(response, 16 * 1024).await?).map_err(|_| Error::Protocol)
        })
        .await;
        result.map_err(|_| Error::Deadline)?
    }

    pub async fn create_camera(
        &self,
        config: &CameraConfig,
        key: &IdempotencyKey,
    ) -> Result<Camera, Error> {
        self.command(Method::POST, "/api/v2/cameras", config, key, None)
            .await
    }

    pub async fn patch_camera(
        &self,
        id: Uuid,
        config: &CameraPatch,
        key: &IdempotencyKey,
        revision: Counter,
    ) -> Result<Camera, Error> {
        self.command(
            Method::PATCH,
            &format!("/api/v2/cameras/{id}"),
            config,
            key,
            Some(revision),
        )
        .await
    }

    pub async fn delete_camera(
        &self,
        id: Uuid,
        key: &IdempotencyKey,
        revision: Counter,
    ) -> Result<(), Error> {
        let command = CommandHeaders {
            key,
            revision: Some(revision),
        };
        self.send(
            Method::DELETE,
            &format!("/api/v2/cameras/{id}"),
            None,
            Some(command),
            true,
        )
        .await?;
        Ok(())
    }

    pub async fn incident(&self, id: Uuid) -> Result<Incident, Error> {
        self.get(&format!("/api/v2/incidents/{id}")).await
    }

    pub async fn incidents(&self, after: Option<&str>, limit: u16) -> Result<IncidentPage, Error> {
        if after.is_some_and(|cursor| {
            cursor.len() > 80
                || !cursor
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'-')
        }) {
            return Err(Error::Protocol);
        }
        let after = after.map_or_else(String::new, |cursor| format!("&after={cursor}"));
        self.get(&format!("/api/v2/incidents?limit={limit}{after}"))
            .await
    }

    pub async fn snapshot(&self) -> Result<Snapshot, Error> {
        self.get("/api/v2/snapshot").await
    }

    pub async fn alerts(&self) -> Result<AlertFeed, Error> {
        self.get("/api/v2/alerts").await
    }

    pub async fn alert_seen(&self, alert: Counter, attention_cycle: Counter) -> Result<(), Error> {
        self.send(
            Method::POST,
            &format!("/api/v2/alerts/{alert}/seen"),
            Some(body(&AlertReceipt { attention_cycle })?),
            None,
            true,
        )
        .await?;
        Ok(())
    }

    pub async fn acknowledge(
        &self,
        id: Uuid,
        revision: Counter,
        key: &IdempotencyKey,
    ) -> Result<Incident, Error> {
        self.command(
            Method::POST,
            &format!("/api/v2/incidents/{id}/acknowledgements"),
            &AcknowledgeRequest::default(),
            key,
            Some(revision),
        )
        .await
    }

    pub async fn resolve(
        &self,
        id: Uuid,
        revision: Counter,
        key: &IdempotencyKey,
        body: &ResolveRequest,
    ) -> Result<Incident, Error> {
        self.command(
            Method::POST,
            &format!("/api/v2/incidents/{id}/resolution"),
            body,
            key,
            Some(revision),
        )
        .await
    }

    pub async fn feedback(
        &self,
        id: Uuid,
        revision: Counter,
        key: &IdempotencyKey,
        body: &FeedbackRequest,
    ) -> Result<Incident, Error> {
        self.command(
            Method::POST,
            &format!("/api/v2/incidents/{id}/feedback"),
            body,
            key,
            Some(revision),
        )
        .await
    }

    pub async fn silence(
        &self,
        id: Uuid,
        revision: Counter,
        key: &IdempotencyKey,
        body: &SilenceRequest,
    ) -> Result<Incident, Error> {
        self.command(
            Method::POST,
            &format!("/api/v2/alerts/{id}/silence"),
            body,
            key,
            Some(revision),
        )
        .await
    }

    pub async fn evidence(&self, incident: Uuid, packet: Uuid) -> Result<EvidencePacket, Error> {
        self.get(&format!("/api/v2/incidents/{incident}/evidence/{packet}"))
            .await
    }

    pub async fn evidence_image(
        &self,
        incident: Uuid,
        packet: Uuid,
        ordinal: u8,
    ) -> Result<Vec<u8>, Error> {
        self.binary(&format!(
            "/api/v2/incidents/{incident}/evidence/{packet}/{ordinal}"
        ))
        .await
    }

    pub async fn camera_image(&self, camera: Uuid) -> Result<Vec<u8>, Error> {
        self.binary(&format!("/api/v2/cameras/{camera}/snapshot.jpg"))
            .await
    }

    pub async fn issue_pairing(&self, request: &PairingRequest) -> Result<PairingOffer, Error> {
        self.issue_pairing_at("/api/v2/pairing", request).await
    }

    pub async fn issue_proxy_pairing(
        &self,
        request: &PairingRequest,
    ) -> Result<PairingOffer, Error> {
        self.issue_pairing_at("/api/v2/pairing?profile=proxy", request)
            .await
    }

    async fn issue_pairing_at(
        &self,
        path: &str,
        request: &PairingRequest,
    ) -> Result<PairingOffer, Error> {
        self.json(Method::POST, path, Some(body(request)?), None, true)
            .await
    }

    pub async fn devices(&self) -> Result<Vec<Device>, Error> {
        self.get("/api/v2/devices").await
    }

    pub async fn revoke(&self, id: Uuid) -> Result<(), Error> {
        self.send(
            Method::DELETE,
            &format!("/api/v2/devices/{id}"),
            None,
            None,
            true,
        )
        .await?;
        Ok(())
    }

    pub async fn stream_session(&self, request: &StreamRequest) -> Result<StreamGrant, Error> {
        self.json(
            Method::POST,
            "/api/v2/stream-sessions",
            Some(body(request)?),
            None,
            true,
        )
        .await
    }

    pub async fn offer_whep(
        &self,
        id: Uuid,
        offer: String,
    ) -> Result<(Vec<u8>, Option<String>), Error> {
        let path = format!("/api/v2/stream-sessions/{id}/whep");
        let response = self
            .send(
                Method::POST,
                &path,
                Some(("application/sdp", offer.into_bytes())),
                None,
                true,
            )
            .await?;
        if response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            != Some(&path)
        {
            return Err(Error::Protocol);
        }
        let etag = response
            .headers()
            .get(header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        Ok((read(response, 48 * 1024).await?, etag))
    }

    pub async fn close_whep(&self, id: Uuid) -> Result<(), Error> {
        self.send(
            Method::DELETE,
            &format!("/api/v2/stream-sessions/{id}/whep"),
            None,
            None,
            true,
        )
        .await?;
        Ok(())
    }

    async fn binary(&self, path: &str) -> Result<Vec<u8>, Error> {
        let response = self.send(Method::GET, path, None, None, true).await?;
        if response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            != Some("image/jpeg")
        {
            return Err(Error::Protocol);
        }
        read(response, RESPONSE_LIMIT).await
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        self.json(Method::GET, path, None, None, true).await
    }

    async fn command<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        value: &impl Serialize,
        key: &IdempotencyKey,
        revision: Option<Counter>,
    ) -> Result<T, Error> {
        self.json(
            method,
            path,
            Some(body(value)?),
            Some(CommandHeaders { key, revision }),
            true,
        )
        .await
    }

    async fn json<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Body>,
        command: Option<CommandHeaders<'_>>,
        authorized: bool,
    ) -> Result<T, Error> {
        let response = self.send(method, path, body, command, authorized).await?;
        serde_json::from_slice(&read(response, RESPONSE_LIMIT).await?).map_err(|_| Error::Protocol)
    }

    fn request(&self, method: Method, path: &str) -> Result<RequestBuilder, Error> {
        if !path.starts_with('/') || path.starts_with("//") || path.contains('\\') {
            return Err(Error::Endpoint);
        }
        let endpoint = self.endpoint.join(path).map_err(|_| Error::Endpoint)?;
        if endpoint.origin() != self.endpoint.origin() {
            return Err(Error::Endpoint);
        }
        Ok(self.http.request(method, endpoint))
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Body>,
        command: Option<CommandHeaders<'_>>,
        authorized: bool,
    ) -> Result<Response, Error> {
        let creds = if authorized {
            self.credentials.as_ref()
        } else {
            None
        };
        let timeout = if method == Method::POST
            && matches!(path, "/api/v2/source-probes" | "/api/v2/cameras")
            || method == Method::PATCH && path.starts_with("/api/v2/cameras/")
        {
            CAMERA_SETUP_TIMEOUT
        } else {
            TIMEOUT
        };
        let deadline = tokio::time::Instant::now() + timeout;
        let mut token = match creds {
            Some(creds) => Some(
                tokio::time::timeout_at(deadline, creds.token())
                    .await
                    .map_err(|_| Error::Deadline)??,
            ),
            None => None,
        };
        let retry = method == Method::GET || command.is_some();
        let mut renewed = false;
        for attempt in 0..3 {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(Error::Deadline);
            }
            let mut request = self.request(method.clone(), path)?.timeout(remaining);
            if let Some(token) = &token {
                if !valid_token(token) {
                    return Err(Error::Credentials);
                }
                request = request.bearer_auth(token);
            }
            if let Some((content_type, bytes)) = &body {
                request = request
                    .header(header::CONTENT_TYPE, *content_type)
                    .body(bytes.clone());
            }
            if let Some(command) = &command {
                request = request.header("Idempotency-Key", command.key.as_str());
                if let Some(revision) = command.revision {
                    request = request.header(header::IF_MATCH, format!("\"{revision}\""));
                }
            }
            let response = match request.send().await {
                Ok(value) => value,
                Err(error) if retry && attempt == 0 && !error.is_timeout() => {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    continue;
                }
                Err(error) => return Err(Error::transport(error)),
            };
            if response.status().as_u16() == 401
                && !renewed
                && retry
                && let (Some(creds), Some(rejected)) = (creds, token.as_ref())
            {
                let replacement = tokio::time::timeout_at(deadline, creds.renew(rejected))
                    .await
                    .map_err(|_| Error::Deadline)??;
                if &replacement != rejected {
                    token = Some(replacement);
                    renewed = true;
                    continue;
                }
            }
            if retry && attempt < 2 && matches!(response.status().as_u16(), 429 | 502 | 503 | 504) {
                let delay =
                    retry_after(&response).unwrap_or(Duration::from_millis(150u64 << attempt));
                if delay < deadline.saturating_duration_since(tokio::time::Instant::now()) {
                    tokio::time::sleep(delay).await;
                    continue;
                }
            }
            return check(response).await;
        }
        Err(Error::Deadline)
    }
}

type Body = (&'static str, Vec<u8>);

struct CommandHeaders<'a> {
    key: &'a IdempotencyKey,
    revision: Option<Counter>,
}

fn body(value: &impl Serialize) -> Result<Body, Error> {
    Ok((
        "application/json",
        serde_json::to_vec(value).map_err(|_| Error::Protocol)?,
    ))
}

fn http_builder() -> reqwest::ClientBuilder {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(3))
        .read_timeout(Duration::from_secs(25))
}

fn pinned_http(der: &[u8]) -> Result<reqwest::Client, Error> {
    let cert = reqwest::Certificate::from_der(der).map_err(|_| Error::Endpoint)?;
    http_builder()
        .https_only(true)
        .min_tls_version(reqwest::tls::Version::TLS_1_3)
        .tls_certs_only([cert])
        .build()
        .map_err(Error::transport)
}

fn endpoint_url(endpoint: &str, tls: bool) -> Result<Url, Error> {
    let url = Url::parse(endpoint).map_err(|_| Error::Endpoint)?;
    let scheme = if tls { "https" } else { "http" };
    if url.scheme() != scheme
        || url.host().is_none()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(Error::Endpoint);
    }
    if !tls {
        let host = url.host_str().ok_or(Error::Endpoint)?;
        let ip = host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .map_err(|_| Error::Endpoint)?;
        if !ip.is_loopback() {
            return Err(Error::Endpoint);
        }
    }
    Ok(url)
}

fn valid_token(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

async fn check(response: Response) -> Result<Response, Error> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status().as_u16();
    let problem = read(response, 16 * 1024)
        .await
        .ok()
        .and_then(|body| serde_json::from_slice::<Problem>(&body).ok())
        .filter(|problem| problem.status == status)
        .map(Box::new);
    Err(Error::Http { status, problem })
}

async fn read(mut response: Response, limit: usize) -> Result<Vec<u8>, Error> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(Error::Protocol);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(Error::transport)? {
        if body.len() + chunk.len() > limit {
            return Err(Error::Protocol);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn retry_after(response: &Response) -> Option<Duration> {
    let value = response.headers().get(header::RETRY_AFTER)?.to_str().ok()?;
    let seconds = value.parse::<u64>().ok()?;
    Some(Duration::from_secs(seconds))
}
