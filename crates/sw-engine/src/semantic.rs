use std::{
    io,
    net::SocketAddr,
    path::Path,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use serde::Serialize;
use sw_domain::Assessment;
use sw_protocol::{
    Category, Classification, EvidenceImageAvailability, EvidenceImageKind, EvidencePacket,
    EvidenceVisibility, InferenceSource, LocalFacts, LocalState, Observable, ProviderCapabilities,
    SemanticObservation, SemanticReport, SemanticRoute, SemanticState, SemanticStatus,
};
use sw_storage::{
    SemanticClaim, SemanticCompleted, SemanticOutcome, SemanticSchedule, SemanticWork, Store,
};
use tokio::{io::AsyncReadExt, sync::watch};
use uuid::Uuid;

const SDK: &str = "13.3.0";
const CLOUD_MODEL: &str = "gemini-3.5-flash-lite";
const LOCAL_MODEL: &str = "chrome_gemini_nano";
const IMAGE_LIMIT: usize = 384 * 1024;
const PACKET_LIMIT: usize = 5 * IMAGE_LIMIT + 16 * 1024;
const RESPONSE_LIMIT: usize = 12 * 1024;
const REFRESH: Duration = Duration::from_secs(5);
const REQUEST_BUDGET: Duration = Duration::from_secs(12);

pub struct SemanticConfig {
    pub address: SocketAddr,
    pub route: SemanticRoute,
    pub daily_limit: u32,
    pub include_replay: bool,
}

/// The provider receives a bounded evidence packet and masked JPEGs only.
/// The supervisor owns consent, expiry, assessment validation and persistence.
pub trait SemanticProvider: Send + Sync {
    fn capabilities(
        &self,
    ) -> impl Future<Output = Result<ProviderCapabilities, SemanticProviderError>> + Send;

    fn evaluate(
        &self,
        request_id: Uuid,
        packet: Vec<u8>,
    ) -> impl Future<Output = Result<SemanticReport, SemanticProviderError>> + Send;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SemanticProviderError {
    #[error("semantic provider unavailable")]
    Unavailable,
    #[error("invalid semantic provider response")]
    Invalid,
    #[error("semantic provider timed out")]
    Deadline,
}

impl From<SemanticProviderError> for SemanticOutcome {
    fn from(value: SemanticProviderError) -> Self {
        match value {
            SemanticProviderError::Unavailable => Self::Unavailable,
            SemanticProviderError::Invalid => Self::Invalid,
            SemanticProviderError::Deadline => Self::Deadline,
        }
    }
}

pub struct SemanticSupervisor<P = FirebaseProvider> {
    transport: P,
    config: SemanticConfig,
    store: Store,
    status: RwLock<SemanticStatus>,
}

/// Loopback transport for the allowlisted StillWatch Firebase worker.
pub struct FirebaseProvider {
    endpoint: String,
    token: String,
    http: reqwest::Client,
}

#[derive(Serialize)]
struct WirePacket<'a> {
    #[serde(flatten)]
    packet: &'a EvidencePacket,
    // EvidencePacket already contains the descriptive images. The worker receives
    // a separate bounded byte index, with each JPEG attached after the JSON.
    attachments: Vec<Attachment<'a>>,
    timeout_ms: u64,
    route: SemanticRoute,
}

#[derive(Serialize)]
struct Attachment<'a> {
    id: &'a str,
    kind: EvidenceImageKind,
    source_s: f64,
    sha256: &'a str,
    bytes: usize,
}

struct PreparedPacket {
    packet: EvidencePacket,
    images: Vec<Vec<u8>>,
}

impl PreparedPacket {
    fn encode(&self, route: SemanticRoute, timeout: Duration) -> Result<Vec<u8>, SemanticOutcome> {
        let attachments = self
            .packet
            .images
            .iter()
            .zip(&self.images)
            .map(|(image, jpeg)| {
                Ok(Attachment {
                    id: &image.image_id,
                    kind: image.kind,
                    source_s: image.source_s,
                    sha256: image.sha256.as_deref().ok_or(SemanticOutcome::Invalid)?,
                    bytes: jpeg.len(),
                })
            })
            .collect::<Result<Vec<_>, SemanticOutcome>>()?;
        let wire = WirePacket {
            packet: &self.packet,
            attachments,
            timeout_ms: timeout.as_millis() as u64,
            route,
        };
        let metadata = serde_json::to_vec(&wire).map_err(|_| SemanticOutcome::Invalid)?;
        let length = 4 + metadata.len() + self.images.iter().map(Vec::len).sum::<usize>();
        if metadata.len() > RESPONSE_LIMIT || length > PACKET_LIMIT {
            return Err(SemanticOutcome::Invalid);
        }
        let mut body = Vec::with_capacity(length);
        body.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
        body.extend(metadata);
        for jpeg in &self.images {
            body.extend_from_slice(jpeg);
        }
        Ok(body)
    }
}

impl SemanticSupervisor {
    pub async fn new(
        config: SemanticConfig,
        token_file: &Path,
        store: Store,
    ) -> io::Result<Arc<Self>> {
        if !config.address.ip().is_loopback()
            || config.address.port() < 1024
            || !(1..=500).contains(&config.daily_limit)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid semantic worker configuration",
            ));
        }
        let file = tokio::fs::File::open(token_file).await?;
        let metadata = file.metadata().await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "semantic credential file must be private",
                ));
            }
        }
        if !metadata.is_file() || metadata.len() > 128 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid semantic credential file",
            ));
        }
        let mut bytes = Vec::new();
        file.take(129).read_to_end(&mut bytes).await?;
        let token = std::str::from_utf8(&bytes).unwrap_or_default().trim();
        if token.len() != 64
            || !token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid semantic worker credential",
            ));
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .tls_certs_only([])
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(1))
            .build()
            .map_err(io::Error::other)?;
        Ok(Arc::new(Self {
            transport: FirebaseProvider {
                endpoint: format!("http://{}", config.address),
                token: token.into(),
                http: client,
            },
            status: RwLock::new(SemanticStatus {
                state: SemanticState::Connecting,
                route_ready: None,
                last_inference_source: None,
                sdk_version: None,
                cloud_model: None,
                local_model_state: None,
                cloud_route_ready: None,
                app_check_state: None,
                used_today: 0,
                daily_request_limit: config.daily_limit,
            }),
            config,
            store,
        }))
    }
    pub fn disabled() -> SemanticStatus {
        SemanticStatus {
            state: SemanticState::Disabled,
            route_ready: None,
            last_inference_source: None,
            sdk_version: None,
            cloud_model: None,
            local_model_state: None,
            cloud_route_ready: None,
            app_check_state: None,
            used_today: 0,
            daily_request_limit: 0,
        }
    }
}

impl<P: SemanticProvider> SemanticSupervisor<P> {
    pub fn status(&self) -> SemanticStatus {
        self.status
            .read()
            .map(|value| value.clone())
            .unwrap_or_else(|_| SemanticSupervisor::disabled())
    }

    pub async fn run(self: Arc<Self>, mut stop: watch::Receiver<bool>) {
        let lease_owner = Uuid::new_v4();
        let mut changes = self.store.subscribe();
        let mut refresh_at = Instant::now();
        let mut capabilities = None;
        let mut failures = 0u32;
        let mut next_request = Instant::now();
        let mut pending = None;
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        while !*stop.borrow() {
            if refresh_at <= Instant::now() {
                capabilities = tokio::select! {
                    _ = stop.wait_for(|stopping| *stopping) => return,
                    outcome = self.transport.capabilities() => outcome.ok().filter(compatible),
                };
                self.refresh(capabilities.as_ref()).await;
                refresh_at = Instant::now() + REFRESH;
            }
            if Instant::now() >= next_request
                && let Some((work, result)) = &pending
            {
                match self.persist_result(work, result).await {
                    Ok(()) => pending = None,
                    Err(sw_storage::StoreError::Idempotency) => {
                        pending = None;
                        self.degrade();
                    }
                    Err(_) => {
                        self.degrade();
                        next_request = Instant::now() + Duration::from_secs(1);
                    }
                }
            }
            if pending.is_none()
                && Instant::now() >= next_request
                && let Some(info) = capabilities.as_ref()
                && ready(info, self.config.route)
                && !info.busy
            {
                let schedule = SemanticSchedule {
                    owner: lease_owner,
                    route: self.config.route,
                    local_ready: info.local_state == LocalState::Ready,
                    daily_limit: self.config.daily_limit,
                    minimum_interval_ms: 4_000,
                    include_replay: self.config.include_replay,
                };
                match self.store.claim_semantic(schedule).await {
                    Ok(Some(claim)) => {
                        let prepared = tokio::select! {
                            _ = async { let _ = stop.wait_for(|stopping| *stopping).await; } => {
                                let _ = self.store.release_semantic(&claim, false).await;
                                return;
                            }
                            prepared = self.prepare_dispatch(&claim, schedule) => prepared,
                        };
                        if let Some((work, body)) = prepared {
                            let outcome = tokio::select! {
                                _ = async { let _ = stop.wait_for(|stopping| *stopping).await; } => {
                                    let _ = self.store.semantic_failed(&work, SemanticOutcome::Unavailable).await;
                                    return;
                                }
                                outcome = self.evaluate_dispatched(&work, body) => outcome,
                            };
                            if outcome
                                .as_ref()
                                .is_err_and(|error| *error != SemanticOutcome::Stale)
                            {
                                self.degrade();
                                failures = failures.saturating_add(1);
                                next_request =
                                    Instant::now() + Duration::from_secs(1u64 << failures.min(5));
                            } else {
                                failures = 0;
                            }
                            let saved = self.persist_result(&work, &outcome).await;
                            if saved.is_err() {
                                self.degrade();
                                next_request = Instant::now() + Duration::from_secs(1);
                                if !matches!(saved, Err(sw_storage::StoreError::Idempotency)) {
                                    pending = Some((work, outcome));
                                }
                            } else if self.finish_response().await.is_err() {
                                self.degrade();
                                next_request = Instant::now() + Duration::from_secs(1);
                            }
                        } else {
                            next_request = Instant::now() + Duration::from_millis(500);
                        }
                    }
                    Ok(None) => {}
                    Err(_) => {
                        self.degrade();
                        next_request = Instant::now() + Duration::from_secs(1);
                    }
                }
            }
            tokio::select! {
                _ = stop.wait_for(|stopping| *stopping) => return,
                _ = changes.changed() => {},
                _ = tick.tick() => {},
            }
        }
    }

    async fn persist_result(
        &self,
        work: &SemanticWork,
        result: &Result<SemanticReport, SemanticOutcome>,
    ) -> Result<(), sw_storage::StoreError> {
        match result {
            Ok(report) => {
                self.store
                    .save_semantic_response(work, report, project(&work.packet, report))
                    .await
            }
            Err(error) => self.store.semantic_failed(work, *error).await,
        }
    }

    async fn finish_response(&self) -> Result<(), sw_storage::StoreError> {
        if let Some(SemanticCompleted::Applied { source }) =
            self.store.finish_semantic_response().await?
            && let Ok(mut status) = self.status.write()
        {
            status.last_inference_source = Some(
                match source {
                    InferenceSource::InCloud => "in_cloud",
                    InferenceSource::OnDevice => "on_device",
                }
                .into(),
            );
            status.state = if status.used_today < self.config.daily_limit {
                SemanticState::Available
            } else {
                SemanticState::Degraded
            };
        }
        Ok(())
    }

    fn degrade(&self) {
        if let Ok(mut status) = self.status.write() {
            status.state = SemanticState::Degraded;
        }
    }

    async fn prepare_dispatch(
        &self,
        claim: &SemanticClaim,
        schedule: SemanticSchedule,
    ) -> Option<(SemanticWork, PreparedPacket)> {
        let prepared = async {
            let work = self
                .store
                .load_semantic(claim)
                .await
                .map_err(|_| SemanticOutcome::Unavailable)?
                .ok_or(SemanticOutcome::Stale)?;
            let body = self.prepare_packet(&work).await?;
            let used = self
                .store
                .dispatch_semantic(&work, schedule)
                .await
                .map_err(|_| SemanticOutcome::Unavailable)?;
            if let Some(used) = used {
                if let Ok(mut status) = self.status.write() {
                    status.used_today = used;
                }
                Ok(Some((work, body)))
            } else {
                Ok(None)
            }
        }
        .await;
        match prepared {
            Ok(Some(work)) => Some(work),
            result => {
                let expire = matches!(
                    result,
                    Err(SemanticOutcome::Invalid
                        | SemanticOutcome::Stale
                        | SemanticOutcome::Deadline)
                );
                if self.store.release_semantic(claim, expire).await.is_err()
                    || matches!(
                        result,
                        Err(SemanticOutcome::Unavailable | SemanticOutcome::Invalid)
                    )
                {
                    self.degrade();
                }
                None
            }
        }
    }

    async fn refresh(&self, capabilities: Option<&ProviderCapabilities>) {
        let now = utc_ms();
        let used = self
            .store
            .semantic_usage(now.div_euclid(86_400_000) * 86_400_000)
            .await;
        let stored = self.finish_response().await.is_ok();
        if let Ok(mut status) = self.status.write() {
            let route_ready = capabilities.is_some_and(|info| ready(info, self.config.route));
            status.route_ready = Some(route_ready);
            status.state = if stored
                && route_ready
                && used
                    .as_ref()
                    .is_ok_and(|used| *used < self.config.daily_limit)
            {
                SemanticState::Available
            } else {
                SemanticState::Degraded
            };
            status.sdk_version = capabilities.map(|info| info.sdk_version.clone());
            status.cloud_model = capabilities.map(|info| info.model.clone());
            status.cloud_route_ready = capabilities.map(|info| info.cloud_transport);
            status.app_check_state = capabilities.and_then(|info| info.app_check_state);
            status.local_model_state = capabilities.map(|info| {
                match info.local_state {
                    LocalState::Ready => "ready",
                    LocalState::Initializing => "initializing",
                    LocalState::Unavailable => "unavailable",
                    LocalState::Disconnected => "disconnected",
                }
                .into()
            });
            if let Ok(used) = used {
                status.used_today = used;
            }
        }
    }

    async fn prepare_packet(&self, work: &SemanticWork) -> Result<PreparedPacket, SemanticOutcome> {
        let deadline_ms = work.stamp.expires_at.saturating_sub(utc_ms());
        let duration = Duration::from_millis(u64::try_from(deadline_ms).unwrap_or_default())
            .min(REQUEST_BUDGET);
        if duration < Duration::from_millis(400) {
            return Err(SemanticOutcome::Deadline);
        }
        tokio::time::timeout(duration, async {
            let mut bodies = Vec::with_capacity(work.packet.images.len());
            if !(1..=5).contains(&work.packet.images.len()) {
                return Err(SemanticOutcome::Invalid);
            }
            for image in &work.packet.images {
                if image.availability != EvidenceImageAvailability::Available {
                    return Err(SemanticOutcome::Invalid);
                }
                let jpeg = self
                    .store
                    .read_evidence(
                        work.packet.incident_id,
                        work.packet.packet_id,
                        image.ordinal,
                    )
                    .await
                    .map_err(|_| SemanticOutcome::Unavailable)?
                    .ok_or(SemanticOutcome::Unavailable)?;
                if image
                    .sha256
                    .as_deref()
                    .is_none_or(|digest| digest.len() != 64)
                    || jpeg.len() > IMAGE_LIMIT
                {
                    return Err(SemanticOutcome::Invalid);
                }
                bodies.push(jpeg);
            }
            let mut packet = work.packet.clone();
            packet.observed_host_utc = None;
            packet.persisted_at = None;
            packet.fresh_when_persisted = None;
            Ok(PreparedPacket {
                packet,
                images: bodies,
            })
        })
        .await
        .map_err(|_| SemanticOutcome::Deadline)?
    }

    async fn evaluate_dispatched(
        &self,
        work: &SemanticWork,
        packet: PreparedPacket,
    ) -> Result<SemanticReport, SemanticOutcome> {
        if !self
            .store
            .semantic_current(work)
            .await
            .map_err(|_| SemanticOutcome::Unavailable)?
        {
            return Err(SemanticOutcome::Stale);
        }
        let now = utc_ms();
        let monotonic = Instant::now();
        if !work.observed.evidence_fresh(now, monotonic) {
            return Err(SemanticOutcome::Stale);
        }
        let remaining = Duration::from_millis(work.stamp.expires_at.saturating_sub(now) as u64)
            .min(REQUEST_BUDGET);
        if remaining < Duration::from_millis(400) {
            return Err(SemanticOutcome::Stale);
        }
        let body = packet.encode(work.route, remaining)?;
        let report =
            tokio::time::timeout(remaining, self.transport.evaluate(work.request_id, body))
                .await
                .map_err(|_| SemanticOutcome::Deadline)??;
        validate(&work.packet, work.route, &report)?;
        Ok(report)
    }
}

impl SemanticProvider for FirebaseProvider {
    async fn capabilities(&self) -> Result<ProviderCapabilities, SemanticProviderError> {
        let response = self
            .http
            .get(format!("{}/health", self.endpoint))
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .map_err(|_| SemanticProviderError::Unavailable)?;
        let bytes = read_response(response).await?;
        serde_json::from_slice(&bytes).map_err(|_| SemanticProviderError::Invalid)
    }

    async fn evaluate(
        &self,
        request_id: Uuid,
        body: Vec<u8>,
    ) -> Result<SemanticReport, SemanticProviderError> {
        let response = self
            .http
            .post(format!("{}/evaluate", self.endpoint))
            .bearer_auth(&self.token)
            .header("Content-Type", "application/octet-stream")
            .header("X-StillWatch-Request-Id", request_id.to_string())
            .body(body)
            .send()
            .await
            .map_err(|_| SemanticProviderError::Unavailable)?;
        let bytes = read_response(response).await?;
        serde_json::from_slice(&bytes).map_err(|_| SemanticProviderError::Invalid)
    }
}

fn compatible(info: &ProviderCapabilities) -> bool {
    info.semantic_evaluation
        && info.sdk_version == SDK
        && info.model == CLOUD_MODEL
        && info.max_images == 5
        && info.max_bytes == PACKET_LIMIT
}

async fn read_response(mut response: reqwest::Response) -> Result<Vec<u8>, SemanticProviderError> {
    if !response.status().is_success() {
        return Err(if response.status() == 504 {
            SemanticProviderError::Deadline
        } else {
            SemanticProviderError::Unavailable
        });
    }
    if response
        .content_length()
        .is_some_and(|length| length > RESPONSE_LIMIT as u64)
    {
        return Err(SemanticProviderError::Invalid);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| SemanticProviderError::Unavailable)?
    {
        if bytes.len() + chunk.len() > RESPONSE_LIMIT {
            return Err(SemanticProviderError::Invalid);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn ready(info: &ProviderCapabilities, route: SemanticRoute) -> bool {
    match route {
        SemanticRoute::CloudOnly => info.cloud_transport,
        SemanticRoute::LocalOnly => info.local_state == LocalState::Ready,
        SemanticRoute::PreferLocal => info.local_state == LocalState::Ready || info.cloud_transport,
    }
}

fn validate(
    packet: &EvidencePacket,
    route: SemanticRoute,
    report: &SemanticReport,
) -> Result<(), SemanticOutcome> {
    let observation = &report.observation;
    let route_ok = match report.inference_source {
        InferenceSource::InCloud => {
            route != SemanticRoute::LocalOnly && report.model == CLOUD_MODEL
        }
        InferenceSource::OnDevice => {
            route != SemanticRoute::CloudOnly && report.model == LOCAL_MODEL
        }
    };
    let category_ok = match (packet.category, observation.observations) {
        (Category::Fire, SemanticObservation::Fire(_)) => matches!(
            observation.classification,
            Classification::NoVisibleFire
                | Classification::LikelyControlledFlame
                | Classification::PossibleUncontrolledFire
                | Classification::LikelyUncontrolledFire
                | Classification::InsufficientEvidence
        ),
        (Category::Medical, SemanticObservation::Medical(_)) => matches!(
            observation.classification,
            Classification::NoVisibleMedicalConcern
                | Classification::PossibleVisibleMedicalConcern
                | Classification::LikelyVisibleMedicalConcern
                | Classification::InsufficientEvidence
        ),
        _ => false,
    };
    let ids = &observation.supporting_image_ids;
    if !route_ok
        || !category_ok
        || report.sdk_version != SDK
        || ids.len() > 5
        || ids.iter().enumerate().any(|(index, id)| {
            ids[..index].contains(id)
                || !packet.images.iter().any(|image| {
                    &image.image_id == id
                        && image.availability == EvidenceImageAvailability::Available
                })
        })
        || observation.contradictions.len() > 5
        || observation.limitations.len() > 5
        || observation.brief_description.chars().count() > 320
        || observation.brief_description.chars().any(char::is_control)
    {
        return Err(SemanticOutcome::Invalid);
    }
    Ok(())
}

fn project(packet: &EvidencePacket, report: &SemanticReport) -> Assessment {
    let observed = &report.observation;
    let latest_overview = packet
        .images
        .iter()
        .rfind(|image| image.kind == EvidenceImageKind::Overview);
    if !observed.contradictions.is_empty()
        || !latest_overview
            .is_some_and(|image| observed.supporting_image_ids.contains(&image.image_id))
    {
        return Assessment::Unknown;
    }
    match (observed.classification, observed.observations) {
        (Classification::LikelyUncontrolledFire, SemanticObservation::Fire(fire)) => {
            let local_spread = packet.context.as_ref().is_some_and(|context| {
                matches!(
                    context.local_facts,
                    LocalFacts::Fire {
                        apparent_growth: true,
                        ..
                    } | LocalFacts::Fire {
                        escaped_controlled_region: true,
                        ..
                    } | LocalFacts::Fire {
                        flame_visible: true,
                        smoke_growth: true,
                        ..
                    }
                )
            });
            if local_spread
                && fire.visibility == EvidenceVisibility::Good
                && fire.flame_visible == Observable::Yes
                && fire.apparent_spread == Observable::Yes
            {
                Assessment::ImmediateVisibleConcern
            } else if fire.visibility != EvidenceVisibility::Poor {
                Assessment::Concern
            } else {
                Assessment::Unknown
            }
        }
        (Classification::PossibleUncontrolledFire, SemanticObservation::Fire(fire))
            if fire.visibility != EvidenceVisibility::Poor =>
        {
            Assessment::Concern
        }
        (
            Classification::PossibleVisibleMedicalConcern
            | Classification::LikelyVisibleMedicalConcern,
            SemanticObservation::Medical(medical),
        ) if medical.visibility != EvidenceVisibility::Poor
            && medical.person_visible == Observable::Yes =>
        {
            Assessment::Concern
        }
        _ => Assessment::Unknown,
    }
}

fn utc_ms() -> i64 {
    (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}
