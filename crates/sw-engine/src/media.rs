use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{self, Read, Write},
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use ring::hmac;
use serde_json::json;
use sha2::{Digest, Sha256};
use sw_domain::SourceScope;
use sw_protocol::{
    Counter, IceServer, MediaStatus, StreamGrant, StreamProfile, StreamRequest, UtcTimestamp,
};
use sw_storage::Store;
use tempfile::TempDir;
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    process::ChildStdin,
    sync::{Semaphore, watch},
};
use uuid::Uuid;

use crate::{CameraFrame, CameraRegistry, capture_process::reap, child_process::command};

const SDP_LIMIT: usize = 48 * 1024;
const MAX_SESSIONS: usize = 32;
const MAX_ENCODERS: usize = 8;
const MAX_SNAPSHOT_ENCODERS: usize = 4;
const STAGE_PREFIX: &str = ".stillwatch-media-";
const STAGE_SUFFIX_LEN: usize = 6;
const ROUTER_CONFIG: &str = "mediamtx.json";
const GRANT_TTL: Duration = Duration::from_secs(20);
const SESSION_TTL: Duration = Duration::from_secs(30 * 60);
const STALE: Duration = Duration::from_secs(3);

#[derive(Debug, Error)]
pub enum MediaError {
    #[error("invalid media configuration or request")]
    Invalid,
    #[error("camera frames or media router unavailable")]
    Unavailable,
    #[error("stream session is missing, expired or revoked")]
    Missing,
    #[error("media capacity reached")]
    Capacity,
    #[error("stream session already has an offer")]
    Conflict,
    #[error("media router rejected the offer")]
    Offer,
    #[error("media I/O failed")]
    Io(#[from] io::Error),
}

pub struct MediaConfig {
    pub mediamtx: PathBuf,
    pub mediamtx_sha256: String,
    pub encoder: PathBuf,
    pub encoder_sha256: String,
    pub ice_udp: SocketAddr,
    pub ice_tcp: Option<SocketAddr>,
    pub advertised_ip: IpAddr,
    pub turn: Option<TurnConfig>,
}

pub struct TurnConfig {
    url: String,
    secret: String,
}

impl TurnConfig {
    pub fn load(url: String, file: &Path) -> Result<Self, MediaError> {
        let (scheme, address) = url.split_once(':').ok_or(MediaError::Invalid)?;
        let (authority, query) = address.split_once('?').unwrap_or((address, ""));
        let safe_host = authority.len() <= 260
            && authority.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'[' | b']' | b':' | b'.' | b'-')
            });
        if !matches!(scheme, "turn" | "turns")
            || !safe_host
            || !matches!(query, "" | "transport=udp" | "transport=tcp")
            || (scheme == "turns" && query == "transport=udp")
            || !authority.rsplit_once(':').is_some_and(|(host, port)| {
                !host.is_empty() && port.parse::<u16>().is_ok_and(|port| port != 0)
            })
        {
            return Err(MediaError::Invalid);
        }
        let info = std::fs::symlink_metadata(file)?;
        if !info.file_type().is_file() || info.len() > 257 {
            return Err(MediaError::Invalid);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if info.permissions().mode() & 0o077 != 0 {
                return Err(MediaError::Invalid);
            }
        }
        let mut bytes = String::new();
        std::io::Read::take(File::open(file)?, 258).read_to_string(&mut bytes)?;
        let secret = bytes.trim().to_owned();
        if bytes.len() > 257
            || !(32..=256).contains(&secret.len())
            || !secret.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(MediaError::Invalid);
        }
        Ok(Self { url, secret })
    }

    fn issue(&self, now_ms: i64) -> IceServer {
        let expires = now_ms.div_euclid(1_000).saturating_add(40 * 60);
        let username = format!("{expires}:{}", Uuid::new_v4().simple());
        // Coturn's temporary credential protocol specifies HMAC-SHA1.
        let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, self.secret.as_bytes());
        let credential = STANDARD.encode(hmac::sign(&key, username.as_bytes()));
        IceServer {
            url: self.url.clone(),
            username,
            credential,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct StreamKey {
    scope: SourceScope,
    profile: StreamProfile,
}

struct Viewer {
    actor: Uuid,
    key: StreamKey,
    expires: Instant,
    phase: ViewerPhase,
}

enum ViewerPhase {
    Granted,
    Negotiating,
    Started { route: String, etag: Option<String> },
}

struct Publisher {
    path: String,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

struct State {
    ready: bool,
    viewers: HashMap<Uuid, Viewer>,
    publishers: HashMap<StreamKey, Publisher>,
}

pub struct MediaAnswer {
    pub sdp: Vec<u8>,
    pub etag: Option<String>,
}

pub struct MediaService {
    config: MediaConfig,
    encoder: H264Encoder,
    cameras: Arc<CameraRegistry>,
    store: Store,
    stage: TempDir,
    webrtc: SocketAddr,
    ingest: SocketAddr,
    read_secret: String,
    http: reqwest::Client,
    snapshots: Arc<Semaphore>,
    state: Mutex<State>,
}

impl MediaService {
    /// Call after acquiring the data directory's instance lock.
    pub fn remove_abandoned_configs(root: &Path) -> io::Result<()> {
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let name = entry.file_name();
            let matches = name
                .to_str()
                .and_then(|name| name.strip_prefix(STAGE_PREFIX))
                .is_some_and(|suffix| {
                    suffix.len() == STAGE_SUFFIX_LEN
                        && suffix.bytes().all(|b| b.is_ascii_alphanumeric())
                });
            if !matches || !entry.file_type()?.is_dir() {
                continue;
            }
            let dir = entry.path();
            let config = dir.join(ROUTER_CONFIG);
            match fs::symlink_metadata(&config) {
                Ok(meta) if meta.is_file() => match fs::remove_file(config) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                },
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            match fs::remove_dir(dir) {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    pub fn status(&self) -> MediaStatus {
        MediaStatus {
            router_ready: self.state.lock().is_ok_and(|state| state.ready),
            video_codec: "h264".into(),
            redacted_transcode: true,
        }
    }

    pub async fn open(
        root: &Path,
        mut config: MediaConfig,
        cameras: Arc<CameraRegistry>,
        store: Store,
    ) -> Result<Arc<Self>, MediaError> {
        if config.ice_udp.port() < 1024
            || config.ice_udp.ip() != config.advertised_ip
            || config
                .ice_tcp
                .is_some_and(|addr| addr.port() < 1024 || addr.ip() != config.advertised_ip)
        {
            return Err(MediaError::Invalid);
        }
        config.mediamtx = verified(&config.mediamtx, &config.mediamtx_sha256)?;
        config.encoder = verified(&config.encoder, &config.encoder_sha256)?;
        let encoder = H264Encoder::detect(&config.encoder).await?;
        let stage = tempfile::Builder::new()
            .prefix(STAGE_PREFIX)
            .rand_bytes(STAGE_SUFFIX_LEN)
            .tempdir_in(root)?;
        let rtc_reservation = TcpListener::bind(("127.0.0.1", 0)).await?;
        let ingest_reservation = TcpListener::bind(("127.0.0.1", 0)).await?;
        let webrtc = rtc_reservation.local_addr()?;
        let ingest = ingest_reservation.local_addr()?;
        let read_secret = random_token()?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .tls_certs_only([])
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_millis(500))
            .timeout(Duration::from_secs(8))
            .build()
            .map_err(io::Error::other)?;
        let service = Arc::new(Self {
            config,
            encoder,
            cameras,
            store,
            stage,
            webrtc,
            ingest,
            read_secret,
            http,
            snapshots: Arc::new(Semaphore::new(MAX_SNAPSHOT_ENCODERS)),
            state: Mutex::new(State {
                ready: false,
                viewers: HashMap::new(),
                publishers: HashMap::new(),
            }),
        });
        service.write_config()?;
        Ok(service)
    }

    fn write_config(&self) -> Result<(), MediaError> {
        let read_hash = STANDARD.encode(Sha256::digest(self.read_secret.as_bytes()));
        let config = json!({
            "logLevel": "error",
            "logDestinations": ["stdout"],
            "readTimeout": "5s",
            "writeTimeout": "5s",
            "api": false, "metrics": false, "pprof": false, "playback": false,
            "rtsp": true, "rtspTransports": ["tcp"], "rtspAddress": self.ingest,
            "rtmp": false, "hls": false, "srt": false, "moq": false,
            "webrtc": true, "webrtcAddress": self.webrtc, "webrtcAllowOrigins": [],
            "webrtcLocalUDPAddress": self.config.ice_udp,
            "webrtcLocalTCPAddress": self.config.ice_tcp.map(|addr| addr.to_string()).unwrap_or_default(),
            "webrtcIPsFromInterfaces": false, "webrtcAdditionalHosts": [self.config.advertised_ip],
            "webrtcHandshakeTimeout": "6s",
            "webrtcICEServers2": self.config.turn.iter().map(|turn| {
                json!({"url": turn.url, "username": "AUTH_SECRET"})
            }).collect::<Vec<_>>(),
            "authInternalUsers": [
                { "user": "any", "ips": ["127.0.0.1"], "permissions": [{"action": "publish"}] },
                { "user": "stillwatch-reader", "pass": format!("sha256:{read_hash}"),
                  "ips": ["127.0.0.1"], "permissions": [{"action": "read"}] }
            ],
            "paths": { "all_others": { "overridePublisher": false } }
        });
        let path = self.stage.path().join(ROUTER_CONFIG);
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.write_all(&serde_json::to_vec(&config).map_err(io::Error::other)?)?;
        file.sync_all()?;
        Ok(())
    }

    pub async fn run(self: Arc<Self>, mut stop: watch::Receiver<bool>) {
        while !*stop.borrow() {
            let mut process = command(&self.config.mediamtx, |command| {
                command
                    .arg(self.stage.path().join(ROUTER_CONFIG))
                    .current_dir(self.stage.path());
                // MediaMTX loads these two prefixes after reading the file. Protect
                // the loopback listeners and auth settings from inherited overrides.
                for (name, _) in std::env::vars_os() {
                    let key = name.to_string_lossy().to_ascii_uppercase();
                    if key.starts_with("MTX_") || key.starts_with("RTSP_") {
                        command.env_remove(name);
                    }
                }
                if let Some(turn) = &self.config.turn {
                    command.env("MTX_WEBRTCICESERVERS2_0_PASSWORD", &turn.secret);
                }
            })
            .spawn();
            if let Ok(child) = &mut process {
                let started = tokio::time::timeout(Duration::from_secs(5), async {
                    while tokio::net::TcpStream::connect(self.webrtc).await.is_err() {
                        if child.try_wait().ok().flatten().is_some() {
                            return false;
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    true
                })
                .await
                .unwrap_or(false);
                if started {
                    if let Ok(mut state) = self.state.lock() {
                        state.ready = true;
                    }
                    let mut tick = tokio::time::interval(Duration::from_millis(500));
                    loop {
                        tokio::select! {
                            _ = async { let _ = stop.wait_for(|stopping| *stopping).await; } => break,
                            _ = child.wait() => break,
                            _ = tick.tick() => self.reap_stale().await,
                        }
                    }
                }
                let _ = reap(&mut **child).await;
            }
            self.clear();
            tokio::select! {
                _ = stop.wait_for(|stopping| *stopping) => return,
                _ = tokio::time::sleep(Duration::from_secs(2)) => {},
            }
        }
    }

    fn clear(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.ready = false;
            state.viewers.clear();
            for (_, publisher) in state.publishers.drain() {
                publisher.stop.send_replace(true);
            }
        }
    }

    pub fn issue(
        self: &Arc<Self>,
        actor: Uuid,
        request: StreamRequest,
        now_ms: i64,
    ) -> Result<StreamGrant, MediaError> {
        let frame = self
            .cameras
            .latest(request.camera_id)
            .filter(|frame| usable(frame))
            .ok_or(MediaError::Unavailable)?;
        let key = StreamKey {
            scope: frame.scope,
            profile: request.profile,
        };
        let mut state = self.state.lock().map_err(|_| MediaError::Unavailable)?;
        if !state.ready {
            return Err(MediaError::Unavailable);
        }
        if state.viewers.len() >= MAX_SESSIONS
            || state
                .viewers
                .values()
                .filter(|item| item.actor == actor)
                .count()
                >= 8
        {
            return Err(MediaError::Capacity);
        }
        if !state.publishers.contains_key(&key) {
            if state.publishers.len() >= MAX_ENCODERS {
                return Err(MediaError::Capacity);
            }
            let (stop, shutdown) = watch::channel(false);
            let service = self.clone();
            let path = format!("cam-{}", random_token()?);
            let target = path.clone();
            let task = tokio::spawn(async move {
                service.publish(key, &target, shutdown).await;
            });
            state.publishers.insert(key, Publisher { path, stop, task });
        }
        let id = Uuid::new_v4();
        state.viewers.insert(
            id,
            Viewer {
                actor,
                key,
                expires: Instant::now() + GRANT_TTL,
                phase: ViewerPhase::Granted,
            },
        );
        Ok(StreamGrant {
            id,
            camera_id: request.camera_id,
            profile: request.profile,
            source_epoch: Counter(key.scope.epoch),
            scene_revision: Counter(key.scope.scene_revision),
            whep_path: format!("/api/v2/stream-sessions/{id}/whep"),
            expires_at: UtcTimestamp::from_unix_millis(
                now_ms.saturating_add(GRANT_TTL.as_millis() as i64),
            )
            .map_err(|_| MediaError::Invalid)?,
            redacted: true,
            codec: "h264".into(),
            ice_servers: self
                .config
                .turn
                .iter()
                .map(|turn| turn.issue(now_ms))
                .collect(),
        })
    }

    pub async fn offer(
        &self,
        actor: Uuid,
        id: Uuid,
        sdp: String,
    ) -> Result<MediaAnswer, MediaError> {
        if sdp.len() > SDP_LIMIT || !sdp.starts_with("v=0") || !sdp.contains("\nm=video ") {
            return Err(MediaError::Invalid);
        }
        let (key, path) = {
            let mut state = self.state.lock().map_err(|_| MediaError::Unavailable)?;
            let viewer = authorized(&mut state, actor, id)?;
            if !matches!(viewer.phase, ViewerPhase::Granted) {
                return Err(MediaError::Conflict);
            }
            viewer.phase = ViewerPhase::Negotiating;
            let key = viewer.key;
            let publisher = state
                .publishers
                .get(&key)
                .filter(|publisher| !publisher.task.is_finished())
                .ok_or(MediaError::Unavailable)?;
            (key, publisher.path.clone())
        };
        let result = self.negotiate(key, &path, sdp).await;
        match result {
            Ok((route, answer)) => {
                let accepted = if let Ok(mut state) = self.state.lock()
                    && let Ok(viewer) = authorized(&mut state, actor, id)
                {
                    viewer.expires = Instant::now() + SESSION_TTL;
                    viewer.phase = ViewerPhase::Started {
                        route: route.clone(),
                        etag: answer.etag.clone(),
                    };
                    true
                } else {
                    false
                };
                if accepted {
                    Ok(answer)
                } else {
                    self.delete_route(&route).await;
                    Err(MediaError::Missing)
                }
            }
            Err(error) => {
                if let Ok(mut state) = self.state.lock() {
                    state.viewers.remove(&id);
                }
                Err(error)
            }
        }
    }

    async fn negotiate(
        &self,
        key: StreamKey,
        path: &str,
        sdp: String,
    ) -> Result<(String, MediaAnswer), MediaError> {
        let url = format!("http://{}/{path}/whep", self.webrtc);
        for attempt in 0..12 {
            if !self.camera_frame(key).is_some_and(|frame| usable(&frame)) {
                return Err(MediaError::Unavailable);
            }
            let mut response = self
                .http
                .post(&url)
                .basic_auth("stillwatch-reader", Some(&self.read_secret))
                .header("Content-Type", "application/sdp")
                .body(sdp.clone())
                .send()
                .await
                .map_err(|_| MediaError::Unavailable)?;
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                if attempt == 11 {
                    return Err(MediaError::Unavailable);
                }
                tokio::time::sleep(Duration::from_millis(150)).await;
                continue;
            }
            if response.status() != 201 {
                return Err(MediaError::Offer);
            }
            let route = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| valid_route(path, value))
                .ok_or(MediaError::Offer)?;
            let etag = response
                .headers()
                .get("etag")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let mut answer = Vec::new();
            loop {
                let part = match response.chunk().await {
                    Ok(Some(part)) => part,
                    Ok(None) => break,
                    Err(_) => {
                        self.delete_route(&route).await;
                        return Err(MediaError::Offer);
                    }
                };
                if answer.len() + part.len() > SDP_LIMIT {
                    self.delete_route(&route).await;
                    return Err(MediaError::Offer);
                }
                answer.extend_from_slice(&part);
            }
            return Ok((route, MediaAnswer { sdp: answer, etag }));
        }
        Err(MediaError::Unavailable)
    }

    pub async fn trickle(
        &self,
        actor: Uuid,
        id: Uuid,
        etag: &str,
        fragment: Vec<u8>,
    ) -> Result<(), MediaError> {
        if fragment.len() > 16 * 1024 || fragment.is_empty() || etag.len() > 128 {
            return Err(MediaError::Invalid);
        }
        let route = {
            let mut state = self.state.lock().map_err(|_| MediaError::Unavailable)?;
            let viewer = authorized(&mut state, actor, id)?;
            let ViewerPhase::Started {
                route,
                etag: expected,
            } = &viewer.phase
            else {
                return Err(MediaError::Conflict);
            };
            if expected.as_deref().is_some_and(|value| value != etag) && etag != "*" {
                return Err(MediaError::Conflict);
            }
            route.clone()
        };
        let response = self
            .http
            .patch(format!("http://{}{route}", self.webrtc))
            .basic_auth("stillwatch-reader", Some(&self.read_secret))
            .header("If-Match", etag)
            .header("Content-Type", "application/trickle-ice-sdpfrag")
            .body(fragment)
            .send()
            .await
            .map_err(|_| MediaError::Unavailable)?;
        if response.status() == 204 {
            Ok(())
        } else {
            Err(MediaError::Offer)
        }
    }

    pub async fn close(&self, actor: Uuid, id: Uuid) -> Result<(), MediaError> {
        let viewer = {
            let mut state = self.state.lock().map_err(|_| MediaError::Unavailable)?;
            authorized(&mut state, actor, id)?;
            state.viewers.remove(&id)
        };
        if let Some(Viewer {
            phase: ViewerPhase::Started { route, .. },
            ..
        }) = viewer
        {
            self.delete_route(&route).await;
        }
        Ok(())
    }

    pub async fn revoke(&self, actor: Uuid) {
        self.remove_where(|viewer| viewer.actor == actor).await;
    }

    pub async fn snapshot(&self, camera: Uuid) -> Result<(Vec<u8>, u64), MediaError> {
        let permit = Arc::clone(&self.snapshots)
            .try_acquire_owned()
            .map_err(|_| MediaError::Capacity)?;
        let frame = self
            .cameras
            .latest(camera)
            .filter(|frame| usable(frame))
            .ok_or(MediaError::Unavailable)?;
        let captured_at = frame.captured_at;
        let scope = frame.scope;
        let jpeg = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            crate::evidence::encode(&frame, None)
        })
        .await
        .map_err(|_| MediaError::Unavailable)?
        .ok_or(MediaError::Unavailable)?;
        let current = self
            .store
            .camera(camera)
            .await
            .map_err(|error| match error {
                sw_storage::StoreError::NotFound => MediaError::Missing,
                _ => MediaError::Unavailable,
            })?;
        let age = captured_at.elapsed();
        if !current.config.enabled
            || scope.epoch != current.epoch
            || scope.scene_revision != current.scene_revision
            || age > STALE
            || !self
                .cameras
                .latest(camera)
                .is_some_and(|latest| latest.scope == scope && usable(&latest))
        {
            return Err(MediaError::Unavailable);
        }
        let age = u64::try_from(age.as_millis()).unwrap_or(u64::MAX);
        Ok((jpeg, age))
    }

    fn camera_frame(&self, key: StreamKey) -> Option<Arc<CameraFrame>> {
        self.cameras
            .latest(key.scope.camera_id)
            .filter(|frame| frame.scope == key.scope)
    }

    async fn reap_stale(&self) {
        let now = Instant::now();
        let (actors, failed) = self
            .state
            .lock()
            .map(|state| {
                let actors = state
                    .viewers
                    .values()
                    .map(|viewer| viewer.actor)
                    .filter(|actor| *actor != self.store.identity().instance_id)
                    .collect::<HashSet<_>>();
                let failed = state
                    .publishers
                    .iter()
                    .filter(|(_, publisher)| publisher.task.is_finished())
                    .map(|(&key, _)| key)
                    .collect::<HashSet<_>>();
                (actors, failed)
            })
            .unwrap_or_default();
        let invalid = if actors.is_empty() {
            HashSet::new()
        } else {
            self.store
                .revoked_devices(actors.into_iter().collect())
                .await
                .unwrap_or_default()
                .into_iter()
                .collect()
        };
        self.remove_where(|viewer| {
            viewer.expires <= now
                || invalid.contains(&viewer.actor)
                || failed.contains(&viewer.key)
                || self
                    .camera_frame(viewer.key)
                    .is_none_or(|frame| !usable(&frame))
        })
        .await;
    }

    async fn remove_where(&self, predicate: impl Fn(&Viewer) -> bool) {
        let removed = if let Ok(mut state) = self.state.lock() {
            let ids: Vec<_> = state
                .viewers
                .iter()
                .filter(|(_, viewer)| predicate(viewer))
                .map(|(&id, _)| id)
                .collect();
            let removed: Vec<_> = ids
                .into_iter()
                .filter_map(|id| state.viewers.remove(&id))
                .collect();
            let in_use: HashSet<_> = state.viewers.values().map(|viewer| viewer.key).collect();
            state.publishers.retain(|key, publisher| {
                let keep = in_use.contains(key) && !publisher.task.is_finished();
                if !keep {
                    publisher.stop.send_replace(true);
                }
                keep
            });
            removed
        } else {
            Vec::new()
        };
        let mut deletes = tokio::task::JoinSet::new();
        for viewer in removed {
            let ViewerPhase::Started { route, .. } = viewer.phase else {
                continue;
            };
            let http = self.http.clone();
            let url = format!("http://{}{route}", self.webrtc);
            let secret = self.read_secret.clone();
            deletes.spawn(async move { delete_upstream(&http, url, &secret).await });
        }
        while deletes.join_next().await.is_some() {}
    }

    async fn delete_route(&self, route: &str) {
        delete_upstream(
            &self.http,
            format!("http://{}{route}", self.webrtc),
            &self.read_secret,
        )
        .await;
    }

    async fn publish(&self, key: StreamKey, path: &str, mut stop: watch::Receiver<bool>) {
        let Some(frame) = self.camera_frame(key) else {
            return;
        };
        let header = frame.frame.header;
        let dimensions = format!("{}x{}", header.width, header.height);
        drop(frame);
        let (fps, scale, bitrate) = match key.profile {
            StreamProfile::Preview => (
                5,
                "scale=w='max(2,trunc(min(480,iw)/2)*2)':h=-2:flags=fast_bilinear",
                "550k",
            ),
            StreamProfile::Detail => (
                12,
                "scale=w='max(2,trunc(min(1280,iw)/2)*2)':h=-2:flags=fast_bilinear",
                "1800k",
            ),
        };
        let rate = fps.to_string();
        let destination = format!("rtsp://{}/{path}", self.ingest);
        let child = command(&self.config.encoder, |command| {
            command.stdin(Stdio::piped()).args([
                "-nostats",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "rawvideo",
                "-pixel_format",
                "rgb24",
                "-video_size",
                &dimensions,
                "-framerate",
                &rate,
                "-i",
                "pipe:0",
                "-an",
                "-vf",
                scale,
            ]);
            command.args(self.encoder.arguments());
            command.args([
                "-pix_fmt",
                "yuv420p",
                "-threads",
                "2",
                "-g",
                &rate,
                "-b:v",
                bitrate,
                "-f",
                "rtsp",
                "-rtsp_transport",
                "tcp",
                &destination,
            ]);
        })
        .spawn();
        let Ok(mut child) = child else {
            return;
        };
        if let Some(mut stdin) = child.stdin().take() {
            self.write_frames(key, fps, &mut stdin, &mut stop).await;
        }
        let _ = reap(&mut *child).await;
    }

    async fn write_frames(
        &self,
        key: StreamKey,
        fps: u64,
        stdin: &mut ChildStdin,
        stop: &mut watch::Receiver<bool>,
    ) {
        let mut tick = tokio::time::interval(Duration::from_micros(1_000_000 / fps));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = stop.wait_for(|stop| *stop) => return,
                _ = tick.tick() => {},
            }
            let Some(frame) = self.camera_frame(key).filter(|frame| usable(frame)) else {
                return;
            };
            if !matches!(
                tokio::time::timeout(Duration::from_secs(1), stdin.write_all(&frame.frame.rgb))
                    .await,
                Ok(Ok(()))
            ) {
                return;
            }
        }
    }
}

enum H264Encoder {
    OpenH264,
    X264,
}

impl H264Encoder {
    async fn detect(path: &Path) -> Result<Self, MediaError> {
        let mut child = command(path, |command| {
            command
                .stdout(Stdio::piped())
                .args(["-hide_banner", "-encoders"]);
        })
        .spawn()?;
        let stdout = child.stdout().take().ok_or(MediaError::Unavailable)?;
        let mut text = String::new();
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            stdout.take(64 * 1024 + 1).read_to_string(&mut text).await?;
            Ok::<_, io::Error>(text.len() <= 64 * 1024 && child.wait().await?.success())
        })
        .await;
        if !matches!(result, Ok(Ok(true))) {
            let _ = reap(&mut *child).await;
            return Err(MediaError::Unavailable);
        }
        let encoders = || {
            text.lines().filter_map(|line| {
                let mut fields = line.split_ascii_whitespace();
                fields.next().filter(|flags| flags.starts_with('V'))?;
                fields.next()
            })
        };
        if encoders().any(|name| name == "libopenh264") {
            Ok(Self::OpenH264)
        } else if encoders().any(|name| name == "libx264") {
            Ok(Self::X264)
        } else {
            Err(MediaError::Invalid)
        }
    }

    fn arguments(&self) -> &[&'static str] {
        match self {
            Self::OpenH264 => &["-c:v", "libopenh264"],
            Self::X264 => &[
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-profile:v",
                "baseline",
            ],
        }
    }
}

fn verified(path: &Path, expected: &str) -> Result<PathBuf, MediaError> {
    let path = path.canonicalize()?;
    let mut file = File::open(&path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > 256 * 1024 * 1024 {
        return Err(MediaError::Invalid);
    }
    let mut hash = Sha256::new();
    io::copy(&mut file, &mut hash)?;
    if !format!("{:x}", hash.finalize()).eq_ignore_ascii_case(expected) {
        return Err(MediaError::Invalid);
    }
    Ok(path)
}

fn random_token() -> Result<String, MediaError> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| MediaError::Unavailable)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn authorized(state: &mut State, actor: Uuid, id: Uuid) -> Result<&mut Viewer, MediaError> {
    state
        .viewers
        .get_mut(&id)
        .filter(|viewer| viewer.actor == actor && viewer.expires > Instant::now())
        .ok_or(MediaError::Missing)
}

fn usable(frame: &CameraFrame) -> bool {
    frame.usable_for_analysis && frame.captured_at.elapsed() <= STALE
}

async fn delete_upstream(http: &reqwest::Client, url: String, secret: &str) {
    let _ = http
        .delete(url)
        .basic_auth("stillwatch-reader", Some(secret))
        .timeout(Duration::from_millis(500))
        .send()
        .await;
}

fn valid_route(path: &str, route: &str) -> Option<String> {
    let prefix = format!("/{path}/whep/");
    let suffix = route.strip_prefix(&prefix)?;
    Uuid::parse_str(suffix).ok()?;
    Some(route.to_owned())
}
