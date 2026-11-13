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
use sw_storage::{SemanticOutcome, SemanticWork, Store};
use tokio::{io::AsyncReadExt, sync::watch};

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

pub struct SemanticSupervisor {
    transport: HttpProvider,
    config: SemanticConfig,
    store: Store,
    status: RwLock<SemanticStatus>,
}

struct HttpProvider {
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
            transport: HttpProvider {
                endpoint: format!("http://{}", config.address),
                token: token.into(),
                http: client,
            },
            status: RwLock::new(SemanticStatus {
                state: SemanticState::Connecting,
                last_inference_source: None,
                sdk_version: None,
                cloud_model: None,
                local_model_state: None,
                used_today: 0,
                daily_request_limit: config.daily_limit,
            }),
            config,
            store,
        }))
    }

    pub fn status(&self) -> SemanticStatus {
        self.status
            .read()
            .map(|value| value.clone())
            .unwrap_or_else(|_| Self::disabled())
    }

    pub fn disabled() -> SemanticStatus {
        SemanticStatus {
            state: SemanticState::Disabled,
            last_inference_source: None,
            sdk_version: None,
            cloud_model: None,
            local_model_state: None,
            used_today: 0,
            daily_request_limit: 0,
        }
    }

    pub async fn run(self: Arc<Self>, mut stop: watch::Receiver<bool>) {
        let mut changes = self.store.subscribe();
        let mut refresh_at = Instant::now();
        let mut capabilities = None;
        let mut failures = 0u32;
        let mut next_request = Instant::now();
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        while !*stop.borrow() {
            if refresh_at <= Instant::now() {
                capabilities = tokio::select! {
                    _ = stop.wait_for(|stopping| *stopping) => return,
                    outcome = self.transport.capabilities() => outcome.ok(),
                };
                self.refresh(capabilities.as_ref()).await;
                refresh_at = Instant::now() + REFRESH;
            }
            if Instant::now() >= next_request
                && let Some(info) = capabilities.as_ref()
                && ready(info, self.config.route)
                && !info.busy
                && let Ok(Some(work)) = self
                    .store
                    .claim_semantic(
                        self.config.route,
                        info.local_state == LocalState::Ready,
                        utc_ms(),
                        self.config.daily_limit,
                        4_000,
                        self.config.include_replay,
                    )
                    .await
            {
                if let Ok(mut status) = self.status.write() {
                    status.used_today = status.used_today.saturating_add(1);
                }
                let outcome = tokio::select! {
                    _ = stop.wait_for(|stopping| *stopping) => return,
                    outcome = self.evaluate(&work) => outcome,
                };
                match outcome {
                    Ok(report) => {
                        let source = report.inference_source;
                        let assessment = project(&work.packet, &report);
                        let saved = self
                            .store
                            .semantic_completed(work, report, assessment, utc_ms())
                            .await;
                        if let Ok(mut status) = self.status.write() {
                            match saved {
                                Ok(true) => {
                                    status.last_inference_source = Some(
                                        match source {
                                            InferenceSource::InCloud => "in_cloud",
                                            InferenceSource::OnDevice => "on_device",
                                        }
                                        .into(),
                                    );
                                    status.state = SemanticState::Available;
                                }
                                Ok(false) => {}
                                Err(_) => status.state = SemanticState::Degraded,
                            }
                        }
                        failures = 0;
                    }
                    Err(error) => {
                        let _ = self
                            .store
                            .semantic_failed(work.stamp, work.route, error, utc_ms())
                            .await;
                        if error != SemanticOutcome::Stale {
                            if let Ok(mut status) = self.status.write() {
                                status.state = SemanticState::Degraded;
                            }
                            failures = failures.saturating_add(1);
                            next_request = Instant::now()
                                + Duration::from_secs((1u64 << failures.min(6)).min(60));
                        }
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

    async fn refresh(&self, capabilities: Option<&ProviderCapabilities>) {
        let now = utc_ms();
        let used = self
            .store
            .semantic_usage(now.div_euclid(86_400_000) * 86_400_000)
            .await;
        if let Ok(mut status) = self.status.write() {
            status.state = if capabilities.is_some_and(|info| ready(info, self.config.route))
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

    async fn evaluate(&self, work: &SemanticWork) -> Result<SemanticReport, SemanticOutcome> {
        let deadline_ms = work.stamp.expires_at.saturating_sub(utc_ms());
        let duration = Duration::from_millis(u64::try_from(deadline_ms).unwrap_or_default())
            .min(REQUEST_BUDGET);
        if duration < Duration::from_millis(400) {
            return Err(SemanticOutcome::Deadline);
        }
        tokio::time::timeout(duration, async {
            let mut bodies = Vec::with_capacity(work.packet.images.len());
            let mut attachments = Vec::with_capacity(work.packet.images.len());
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
                let sha256 = image.sha256.as_deref().ok_or(SemanticOutcome::Invalid)?;
                if jpeg.len() > IMAGE_LIMIT {
                    return Err(SemanticOutcome::Invalid);
                }
                attachments.push(Attachment {
                    id: &image.image_id,
                    kind: image.kind,
                    source_s: image.source_s,
                    sha256,
                    bytes: jpeg.len(),
                });
                bodies.push(jpeg);
            }
            let wire = WirePacket {
                packet: &work.packet,
                attachments,
                timeout_ms: duration.as_millis() as u64,
                route: work.route,
            };
            let metadata = serde_json::to_vec(&wire).map_err(|_| SemanticOutcome::Invalid)?;
            let length = 4 + metadata.len() + bodies.iter().map(Vec::len).sum::<usize>();
            if metadata.len() > RESPONSE_LIMIT || length > PACKET_LIMIT {
                return Err(SemanticOutcome::Invalid);
            }
            let mut body = Vec::with_capacity(length);
            body.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
            body.extend(metadata);
            for jpeg in bodies {
                body.extend(jpeg);
            }
            if !self
                .store
                .semantic_current(work, utc_ms())
                .await
                .map_err(|_| SemanticOutcome::Unavailable)?
            {
                return Err(SemanticOutcome::Stale);
            }
            let report = self.transport.evaluate(body).await?;
            validate(&work.packet, work.route, &report)?;
            Ok(report)
        })
        .await
        .map_err(|_| SemanticOutcome::Deadline)?
    }
}

impl HttpProvider {
    async fn capabilities(&self) -> Result<ProviderCapabilities, SemanticOutcome> {
        let response = self
            .http
            .get(format!("{}/health", self.endpoint))
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .map_err(|_| SemanticOutcome::Unavailable)?;
        let bytes = read_response(response).await?;
        let info: ProviderCapabilities =
            serde_json::from_slice(&bytes).map_err(|_| SemanticOutcome::Invalid)?;
        if !info.semantic_evaluation
            || info.sdk_version != SDK
            || info.model != CLOUD_MODEL
            || info.max_images != 5
            || info.max_bytes != PACKET_LIMIT
        {
            return Err(SemanticOutcome::Invalid);
        }
        Ok(info)
    }

    async fn evaluate(&self, body: Vec<u8>) -> Result<SemanticReport, SemanticOutcome> {
        let response = self
            .http
            .post(format!("{}/evaluate", self.endpoint))
            .bearer_auth(&self.token)
            .header("Content-Type", "application/octet-stream")
            .body(body)
            .send()
            .await
            .map_err(|_| SemanticOutcome::Unavailable)?;
        let bytes = read_response(response).await?;
        serde_json::from_slice(&bytes).map_err(|_| SemanticOutcome::Invalid)
    }
}

async fn read_response(mut response: reqwest::Response) -> Result<Vec<u8>, SemanticOutcome> {
    if !response.status().is_success() {
        return Err(if response.status() == 504 {
            SemanticOutcome::Deadline
        } else {
            SemanticOutcome::Unavailable
        });
    }
    if response
        .content_length()
        .is_some_and(|length| length > RESPONSE_LIMIT as u64)
    {
        return Err(SemanticOutcome::Invalid);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| SemanticOutcome::Unavailable)?
    {
        if bytes.len() + chunk.len() > RESPONSE_LIMIT {
            return Err(SemanticOutcome::Invalid);
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
