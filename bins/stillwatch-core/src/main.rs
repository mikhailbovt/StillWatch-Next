#![forbid(unsafe_code)]

mod health;
mod lan;
mod network;
mod proxy;
#[cfg(windows)]
mod scm;
mod token;

use std::{
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use clap::Parser;
use sw_engine::{
    CameraRegistry, MediaConfig, MediaService, OnvifConfig, SemanticConfig, SemanticSupervisor,
    SourceConfig, TurnConfig, VisionScheduler, WorkerBinary,
};
use sw_protocol::{
    PairingProfile, SemanticRoute,
    worker::{CudaConfig, InferenceConfig, MODEL_SELECTION_MAX_BYTES, ModelSelection},
};
use sw_storage::{CameraLimits, DEFAULT_DATABASE_QUOTA_BYTES, Store, StoreConfig};
use tokio::{net::TcpListener, sync::watch};

#[derive(Clone, Parser)]
#[command(version, about = "StillWatch autonomous host service")]
struct Config {
    #[arg(long)]
    data_dir: PathBuf,
    #[arg(long, default_value = "127.0.0.1:8765")]
    listen: SocketAddr,
    #[arg(long)]
    lan_listen: Option<SocketAddr>,
    #[arg(long, requires = "lan_listen")]
    lan_advertise: bool,
    #[arg(skip)]
    https_proxy: Option<proxy::ProxySettings>,
    #[command(flatten)]
    media: MediaArgs,
    #[arg(long, default_value_t = 500 * 1024 * 1024)]
    evidence_quota_bytes: u64,
    /// Maximum size in bytes for the SQLite metadata database.
    #[arg(long)]
    database_quota_bytes: Option<u64>,
    #[arg(long, default_value_t = 24, value_parser = clap::value_parser!(u32).range(1..=8_760))]
    evidence_retention_hours: u32,
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=3650))]
    history_retention_days: u32,
    #[arg(long)]
    replay_root: Option<PathBuf>,
    #[arg(long)]
    worker: Option<PathBuf>,
    #[arg(long, default_value_t = 4)]
    active_cameras: u32,
    #[arg(long, default_value_t = 128 * 1024 * 1024)]
    raw_frame_bytes: u32,
    #[arg(long)]
    rtsp_peer: Vec<SocketAddr>,
    /// Local IPv4 interface for optional WS-Discovery multicast.
    #[arg(long)]
    onvif_interface: Option<Ipv4Addr>,
    /// Authorized numeric HTTP(S) device-service URL, including its path.
    #[arg(long)]
    onvif_endpoint: Vec<String>,
    /// PEM trust anchors for the local ONVIF HTTPS cameras.
    #[arg(long)]
    onvif_ca_file: Option<PathBuf>,
    #[arg(long)]
    camera_credentials_dir: Option<PathBuf>,
    #[arg(long, requires_all = ["model_manifest_sha256", "ort_library", "ort_library_sha256"])]
    model_pack: Option<PathBuf>,
    #[arg(long, requires = "model_pack")]
    model_manifest_sha256: Option<String>,
    #[arg(long, requires = "model_pack")]
    ort_library: Option<PathBuf>,
    #[arg(long, requires = "model_pack")]
    ort_library_sha256: Option<String>,
    #[arg(long, requires_all = ["model_pack", "cuda_provider_sha256", "cuda_shared_sha256"])]
    cuda_device: Option<u16>,
    #[arg(long, requires = "cuda_device")]
    cuda_provider_sha256: Option<String>,
    #[arg(long, requires = "cuda_device")]
    cuda_shared_sha256: Option<String>,
    /// Read the administrator's validated model selection at startup.
    #[arg(long)]
    model_selection_file: Option<PathBuf>,
    /// Warm the selected inference worker and check readiness without opening cameras.
    #[arg(long)]
    check_inference: bool,
    /// JSON settings from the installation administrator.
    #[arg(long)]
    settings_file: Option<PathBuf>,
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u8).range(1..=64))]
    inference_threads: u8,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=8))]
    inference_workers: u8,
    #[arg(long, requires = "semantic_token_file")]
    semantic_worker: Option<SocketAddr>,
    #[arg(long, requires = "semantic_worker")]
    semantic_token_file: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "cloud-only")]
    semantic_route: SemanticRouteArg,
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=500))]
    semantic_daily_limit: u32,
    #[arg(long, requires = "semantic_worker")]
    semantic_include_replay: bool,
    #[cfg(windows)]
    #[arg(long)]
    service: bool,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum SemanticRouteArg {
    CloudOnly,
    LocalOnly,
    PreferLocal,
}

#[derive(Clone, clap::Args)]
struct MediaArgs {
    #[arg(long, requires_all = ["mediamtx_sha256", "media_encoder", "media_encoder_sha256", "media_ice_udp"])]
    mediamtx: Option<PathBuf>,
    #[arg(long, requires = "mediamtx")]
    mediamtx_sha256: Option<String>,
    #[arg(long, requires = "mediamtx")]
    media_encoder: Option<PathBuf>,
    #[arg(long, requires = "mediamtx")]
    media_encoder_sha256: Option<String>,
    #[arg(long, requires = "mediamtx")]
    media_ice_udp: Option<SocketAddr>,
    #[arg(long, requires = "mediamtx")]
    media_ice_tcp: Option<SocketAddr>,
    #[arg(long, requires_all = ["mediamtx", "media_turn_secret_file"])]
    media_turn: Option<String>,
    #[arg(long, requires = "media_turn")]
    media_turn_secret_file: Option<PathBuf>,
}

impl From<SemanticRouteArg> for SemanticRoute {
    fn from(route: SemanticRouteArg) -> Self {
        match route {
            SemanticRouteArg::CloudOnly => Self::CloudOnly,
            SemanticRouteArg::LocalOnly => Self::LocalOnly,
            SemanticRouteArg::PreferLocal => Self::PreferLocal,
        }
    }
}

fn main() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut config = Config::parse();
    if let Some(path) = config.settings_file.as_deref() {
        let file = std::fs::File::open(path).context("open Core settings")?;
        let mut bytes = Vec::new();
        use std::io::Read;
        file.take(65_537)
            .read_to_end(&mut bytes)
            .context("read Core settings")?;
        ensure!(bytes.len() <= 65_536, "Core settings exceed 64 KiB");
        let settings: Settings = serde_json::from_slice(&bytes).context("invalid Core settings")?;
        settings.apply(&mut config);
    }
    if let Some(path) = config.model_selection_file.as_deref() {
        use std::io::Read;

        ensure!(
            config.model_pack.is_none()
                && config.model_manifest_sha256.is_none()
                && config.ort_library.is_none()
                && config.ort_library_sha256.is_none()
                && config.cuda_device.is_none()
                && config.cuda_provider_sha256.is_none()
                && config.cuda_shared_sha256.is_none(),
            "configure a model selection file or explicit model runtime settings"
        );
        let file = std::fs::File::open(path).context("open validated model selection")?;
        let mut bytes = Vec::new();
        file.take(MODEL_SELECTION_MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .context("read validated model selection")?;
        ensure!(
            bytes.len() <= MODEL_SELECTION_MAX_BYTES,
            "model selection exceeds the size limit"
        );
        let selected: ModelSelection =
            serde_json::from_slice(&bytes).context("invalid model selection")?;
        ensure!(
            selected.models.is_absolute() && selected.library.is_absolute(),
            "model selection must use absolute paths"
        );
        config.model_pack = Some(selected.models);
        config.model_manifest_sha256 = Some(selected.manifest_sha256);
        config.ort_library = Some(selected.library);
        config.ort_library_sha256 = Some(selected.library_sha256);
        if let Some(cuda) = selected.cuda {
            config.cuda_device = Some(cuda.device);
            config.cuda_provider_sha256 = Some(cuda.provider_sha256);
            config.cuda_shared_sha256 = Some(cuda.shared_sha256);
        }
    }
    if config.check_inference {
        let inference = inference_config(&config)?.context("no model pack configured")?;
        let worker = WorkerBinary::open(&worker_path(&config)?)?;
        let lanes = usize::from(config.inference_workers);
        runtime()?.block_on(sw_engine::verify_inference_startup(
            &worker, &inference, lanes,
        ))?;
        println!("StillWatch inference workers are ready ({lanes})");
        return Ok(());
    }
    #[cfg(windows)]
    if config.service {
        return scm::start(config);
    }
    runtime()?.block_on(foreground(config))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    lan_listen: Option<SocketAddr>,
    lan_advertise: Option<bool>,
    https_proxy: Option<proxy::ProxySettings>,
    model_selection_file: Option<PathBuf>,
    model_pack: Option<PathBuf>,
    model_manifest_sha256: Option<String>,
    ort_library: Option<PathBuf>,
    ort_library_sha256: Option<String>,
    cuda: Option<CudaConfig>,
    #[serde(default)]
    rtsp_peers: Vec<SocketAddr>,
    onvif_interface: Option<Ipv4Addr>,
    #[serde(default)]
    onvif_endpoints: Vec<String>,
    onvif_ca_file: Option<PathBuf>,
    camera_credentials_dir: Option<PathBuf>,
    mediamtx: Option<PathBuf>,
    mediamtx_sha256: Option<String>,
    media_encoder: Option<PathBuf>,
    media_encoder_sha256: Option<String>,
    media_ice_udp: Option<SocketAddr>,
    media_ice_tcp: Option<SocketAddr>,
    media_turn: Option<String>,
    media_turn_secret_file: Option<PathBuf>,
    semantic_worker: Option<SocketAddr>,
    semantic_token_file: Option<PathBuf>,
    semantic_route: Option<SemanticRoute>,
    semantic_include_replay: Option<bool>,
    evidence_retention_hours: Option<u32>,
    history_retention_days: Option<u32>,
    database_quota_bytes: Option<u64>,
}

impl Settings {
    fn apply(self, config: &mut Config) {
        config.lan_listen = config.lan_listen.or(self.lan_listen);
        config.lan_advertise |= self.lan_advertise.unwrap_or(false);
        config.https_proxy = self.https_proxy;
        config.model_selection_file = config
            .model_selection_file
            .take()
            .or(self.model_selection_file);
        config.model_pack = config.model_pack.take().or(self.model_pack);
        config.model_manifest_sha256 = config
            .model_manifest_sha256
            .take()
            .or(self.model_manifest_sha256);
        config.ort_library = config.ort_library.take().or(self.ort_library);
        config.ort_library_sha256 = config.ort_library_sha256.take().or(self.ort_library_sha256);
        if config.cuda_device.is_none()
            && let Some(cuda) = self.cuda
        {
            config.cuda_device = Some(cuda.device);
            config.cuda_provider_sha256 = Some(cuda.provider_sha256);
            config.cuda_shared_sha256 = Some(cuda.shared_sha256);
        }
        config.rtsp_peer.extend(self.rtsp_peers);
        config.onvif_interface = config.onvif_interface.or(self.onvif_interface);
        config.onvif_endpoint.extend(self.onvif_endpoints);
        config.onvif_ca_file = config.onvif_ca_file.take().or(self.onvif_ca_file);
        config.evidence_retention_hours = self
            .evidence_retention_hours
            .unwrap_or(config.evidence_retention_hours);
        config.history_retention_days = self
            .history_retention_days
            .unwrap_or(config.history_retention_days);
        config.database_quota_bytes = config.database_quota_bytes.or(self.database_quota_bytes);
        config.camera_credentials_dir = config
            .camera_credentials_dir
            .take()
            .or(self.camera_credentials_dir);
        config.media.mediamtx = config.media.mediamtx.take().or(self.mediamtx);
        config.media.mediamtx_sha256 = config.media.mediamtx_sha256.take().or(self.mediamtx_sha256);
        config.media.media_encoder = config.media.media_encoder.take().or(self.media_encoder);
        config.media.media_encoder_sha256 = config
            .media
            .media_encoder_sha256
            .take()
            .or(self.media_encoder_sha256);
        config.media.media_ice_udp = config.media.media_ice_udp.or(self.media_ice_udp);
        config.media.media_ice_tcp = config.media.media_ice_tcp.or(self.media_ice_tcp);
        config.media.media_turn = config.media.media_turn.take().or(self.media_turn);
        config.media.media_turn_secret_file = config
            .media
            .media_turn_secret_file
            .take()
            .or(self.media_turn_secret_file);
        if config.semantic_worker.is_none() && config.semantic_token_file.is_none() {
            config.semantic_worker = self.semantic_worker;
            config.semantic_token_file = self.semantic_token_file;
            if let Some(route) = self.semantic_route {
                config.semantic_route = match route {
                    SemanticRoute::CloudOnly => SemanticRouteArg::CloudOnly,
                    SemanticRoute::LocalOnly => SemanticRouteArg::LocalOnly,
                    SemanticRoute::PreferLocal => SemanticRouteArg::PreferLocal,
                };
            }
            config.semantic_include_replay |= self.semantic_include_replay.unwrap_or(false);
        }
    }
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?)
}

async fn foreground(config: Config) -> Result<()> {
    let (stop, shutdown) = watch::channel(false);
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let signals = tokio::spawn(async move {
        #[cfg(unix)]
        tokio::select! {
            _ = terminate.recv() => {},
            _ = tokio::signal::ctrl_c() => {},
        }
        #[cfg(not(unix))]
        let _ = tokio::signal::ctrl_c().await;
        stop.send_replace(true);
    });
    let result = serve(
        config,
        shutdown,
        || {
            #[cfg(target_os = "linux")]
            sd_notify::notify(&[sd_notify::NotifyState::Ready])?;
            Ok(())
        },
        || {
            #[cfg(target_os = "linux")]
            let _ = sd_notify::notify(&[sd_notify::NotifyState::Stopping]);
        },
    )
    .await;
    signals.abort();
    result
}

async fn serve(
    config: Config,
    mut shutdown: watch::Receiver<bool>,
    ready: impl FnOnce() -> Result<()> + Send,
    stopping: impl FnOnce() + Send + 'static,
) -> Result<()> {
    ensure!(
        config.listen.ip().is_loopback(),
        "Core requires a loopback listener for the local trust profile"
    );
    ensure!(
        config.semantic_worker.is_some() == config.semantic_token_file.is_some(),
        "semantic worker requires a token file"
    );
    ensure!(
        config.media.media_turn.is_some() == config.media.media_turn_secret_file.is_some()
            && (config.media.media_turn.is_none() || config.media.mediamtx.is_some()),
        "TURN requires a secret file and configured media service"
    );
    ensure!(
        (1..=8_760).contains(&config.evidence_retention_hours),
        "invalid evidence retention hours"
    );
    let evidence_retention_ms = i64::from(config.evidence_retention_hours) * 3_600_000;
    ensure!(
        (1..=3650).contains(&config.history_retention_days),
        "invalid history retention days"
    );
    let history_retention_ms = i64::from(config.history_retention_days) * 86_400_000;
    let worker = WorkerBinary::open(&worker_path(&config)?).ok();
    if worker.is_none() {
        eprintln!("StillWatch capture worker unavailable");
    }
    let limits = CameraLimits {
        active: config.active_cameras,
        raw_bytes: config.raw_frame_bytes,
    };
    let inference_workers = usize::from(config.inference_workers);
    let inference = inference_config(&config)?;
    let dir = config.data_dir;
    let media_root = dir.clone();
    let storage_root = dir.clone();
    let source_root = config.replay_root;
    let peers = config.rtsp_peer;
    let onvif = OnvifConfig {
        interface: config.onvif_interface,
        device_endpoints: config.onvif_endpoint,
        trusted_ca: config.onvif_ca_file,
    };
    let credentials_root = config.camera_credentials_dir;
    let lan_options = config
        .lan_listen
        .map(|address| (address, config.lan_advertise));
    let capture_worker = worker.clone();
    let (store, digest, registry, lan, proxy) =
        tokio::task::spawn_blocking(move || -> Result<_> {
            let store = Store::open(
                &dir,
                StoreConfig {
                    evidence_quota_bytes: config.evidence_quota_bytes,
                    database_quota_bytes: config
                        .database_quota_bytes
                        .unwrap_or(DEFAULT_DATABASE_QUOTA_BYTES),
                    ..StoreConfig::default()
                },
            )?;
            if MediaService::remove_abandoned_configs(&dir).is_err() {
                eprintln!("StillWatch could not remove abandoned media configurations");
            }
            let digest = token::load_or_create(&dir)?;
            let source_root = match source_root {
                Some(path) => path,
                None => {
                    let root = dir.join("sources");
                    std::fs::create_dir_all(&root)?;
                    root
                }
            };
            let sources = SourceConfig {
                replay_root: source_root,
                rtsp_peers: peers.into_iter().collect(),
                credentials_root,
                onvif,
            };
            let registry = CameraRegistry::new(
                store.clone(),
                capture_worker,
                sources,
                limits,
                inference_workers,
            )?;
            let lan = lan_options
                .map(|(address, advertise)| lan::Lan::open(&dir, address, advertise))
                .transpose()?;
            let proxy = config
                .https_proxy
                .map(|settings| proxy::Proxy::open(settings, &digest))
                .transpose()?;
            Ok((store, digest, registry, lan, proxy))
        })
        .await
        .context("storage startup task terminated")??;
    if *shutdown.borrow() {
        return Ok(());
    }
    let listener = TcpListener::bind(config.listen)
        .await
        .context("unable to bind Core loopback listener")?;
    let address = listener.local_addr()?;
    let semantic = if let (Some(address), Some(token)) =
        (config.semantic_worker, config.semantic_token_file)
    {
        let settings = SemanticConfig {
            address,
            route: config.semantic_route.into(),
            daily_limit: config.semantic_daily_limit,
            include_replay: config.semantic_include_replay,
        };
        Some(
            SemanticSupervisor::new(settings, &token, store.clone())
                .await
                .context("semantic worker configuration")?,
        )
    } else {
        None
    };
    let vision = inference
        .map(|config| VisionScheduler::new(worker, config, registry.clone(), store.clone()));
    let media = if let Some(mediamtx) = config.media.mediamtx {
        let ice_udp = config
            .media
            .media_ice_udp
            .context("media ICE bind address")?;
        let address = lan.as_ref().map_or(config.listen.ip(), |lan| lan.bind_ip());
        ensure!(
            ice_udp.ip() == address,
            "media ICE address must match the Core listener"
        );
        let options = MediaConfig {
            mediamtx,
            mediamtx_sha256: config.media.mediamtx_sha256.context("MediaMTX SHA-256")?,
            encoder: config.media.media_encoder.context("media encoder")?,
            encoder_sha256: config
                .media
                .media_encoder_sha256
                .context("media encoder SHA-256")?,
            ice_udp,
            ice_tcp: config.media.media_ice_tcp,
            advertised_ip: address,
            turn: config
                .media
                .media_turn
                .zip(config.media.media_turn_secret_file)
                .map(|(url, file)| TurnConfig::load(url, &file))
                .transpose()?,
        };
        Some(MediaService::open(&media_root, options, registry.clone(), store.clone()).await?)
    } else {
        None
    };
    let services = sw_api::ApiServices {
        cameras: Some(registry.clone()),
        vision: vision.clone(),
        semantic: semantic.clone(),
        media: media.clone(),
    };
    let endpoints = sw_api::PairingEndpoints {
        lan: lan.as_ref().map(|lan| lan.identity.clone()),
        proxy: proxy.as_ref().map(|proxy| proxy.identity.clone()),
    };
    let lan_network = if let Some(lan) = lan {
        let listener = lan.bind().await?;
        let router = sw_api::network_router(
            store.clone(),
            lan.host.clone(),
            endpoints.clone(),
            PairingProfile::Lan,
            shutdown.clone(),
            services.clone(),
        );
        Some((lan, listener, router))
    } else {
        None
    };
    let proxy_network = if let Some(proxy) = proxy {
        let listener = proxy.bind().await?;
        let router = sw_api::network_router(
            store.clone(),
            proxy.host.clone(),
            endpoints.clone(),
            PairingProfile::Proxy,
            shutdown.clone(),
            services.clone(),
        );
        Some((listener, proxy.protect(router)))
    } else {
        None
    };
    let router = sw_api::local_router(
        store.clone(),
        address,
        digest,
        shutdown.clone(),
        services,
        endpoints,
    )
    .context("invalid local API configuration")?;
    ready()?;
    eprintln!("StillWatch Core listening on {address}");
    let (stop_registry, registry_shutdown) = watch::channel(false);
    let mut maintenance = tokio::spawn(maintain_storage(
        store.clone(),
        storage_root,
        registry_shutdown.clone(),
        evidence_retention_ms,
        history_retention_ms,
    ));
    let mut health_task = tokio::spawn(health::monitor(
        store.clone(),
        registry.clone(),
        vision.clone(),
        semantic.clone(),
        registry_shutdown.clone(),
    ));
    let media_task = media.map(|service| tokio::spawn(service.run(registry_shutdown.clone())));
    let lan_task = lan_network.map(|(lan, listener, router)| {
        let instance = store.identity().instance_id.to_string();
        tokio::spawn(lan.serve(listener, router, instance, registry_shutdown.clone()))
    });
    let proxy_task = proxy_network.map(|(listener, router)| {
        tokio::spawn(network::serve(
            listener,
            router,
            None,
            registry_shutdown.clone(),
        ))
    });
    let mut registry_task = tokio::spawn(registry.supervise(registry_shutdown.clone()));
    let semantic_task =
        semantic.map(|semantic| tokio::spawn(semantic.run(registry_shutdown.clone())));
    let vision_task = vision.map(|vision| tokio::spawn(vision.supervise(registry_shutdown)));
    let signal_registry = stop_registry.clone();

    #[cfg(target_os = "linux")]
    let watchdog = start_watchdog(store);
    let (drained, mut draining) = watch::channel(false);
    let server = axum::serve(listener, router).with_graceful_shutdown(async move {
        let _ = shutdown.wait_for(|stopping| *stopping).await;
        stopping();
        signal_registry.send_replace(true);
        drained.send_replace(true);
    });
    let result = tokio::select! {
        result = async { server.await } => result.context("HTTP server failed"),
        _ = async {
            let _ = draining.wait_for(|draining| *draining).await;
            tokio::time::sleep(Duration::from_secs(10)).await;
        } => Ok(()),
    };
    #[cfg(target_os = "linux")]
    if let Some(watchdog) = watchdog {
        watchdog.abort();
    }
    stop_registry.send_replace(true);
    if tokio::time::timeout(Duration::from_secs(3), &mut health_task)
        .await
        .is_err()
    {
        health_task.abort();
    }
    if tokio::time::timeout(Duration::from_secs(3), &mut maintenance)
        .await
        .is_err()
    {
        maintenance.abort();
    }
    if let Some(mut media_task) = media_task
        && tokio::time::timeout(Duration::from_secs(5), &mut media_task)
            .await
            .is_err()
    {
        media_task.abort();
    }
    for mut task in [lan_task, proxy_task].into_iter().flatten() {
        if tokio::time::timeout(Duration::from_secs(4), &mut task)
            .await
            .is_err()
        {
            task.abort();
        }
    }
    if tokio::time::timeout(Duration::from_secs(7), &mut registry_task)
        .await
        .is_err()
    {
        registry_task.abort();
    }
    if let Some(mut vision_task) = vision_task
        && tokio::time::timeout(Duration::from_secs(5), &mut vision_task)
            .await
            .is_err()
    {
        vision_task.abort();
    }
    if let Some(mut semantic_task) = semantic_task
        && tokio::time::timeout(Duration::from_secs(3), &mut semantic_task)
            .await
            .is_err()
    {
        semantic_task.abort();
    }
    result
}

async fn maintain_storage(
    store: Store,
    storage_root: PathBuf,
    mut stop: watch::Receiver<bool>,
    evidence_retention_ms: i64,
    history_retention_ms: i64,
) {
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    while !*stop.borrow() {
        tokio::select! {
            _ = async { let _ = stop.wait_for(|stopping| *stopping).await; } => return,
            _ = tick.tick() => {
                health::check_disk(&store, &storage_root).await;
                let now = (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64;
                if store.expire_alerts(now).await.is_err() {
                    eprintln!("StillWatch could not expire notification intents");
                }
                if store.retire_evidence(now.saturating_sub(evidence_retention_ms), now).await.is_err() {
                    eprintln!("StillWatch could not apply evidence retention");
                }
                if store.retain_history(now.saturating_sub(history_retention_ms), now).await.is_err() {
                    eprintln!("StillWatch could not apply history retention");
                }
            }
        }
    }
}

fn worker_path(config: &Config) -> Result<PathBuf> {
    if let Some(path) = &config.worker {
        return Ok(path.clone());
    }
    Ok(std::env::current_exe()?.with_file_name(if cfg!(windows) {
        "stillwatch-worker.exe"
    } else {
        "stillwatch-worker"
    }))
}

fn inference_config(config: &Config) -> Result<Option<InferenceConfig>> {
    let inference_workers = usize::from(config.inference_workers);
    let cuda = match (
        config.cuda_device,
        &config.cuda_provider_sha256,
        &config.cuda_shared_sha256,
    ) {
        (Some(device), Some(provider_sha256), Some(shared_sha256)) => {
            ensure!(
                inference_workers == 1 && config.model_pack.is_some(),
                "CUDA requires one worker per GPU and a model pack"
            );
            ensure!(
                valid_digest(provider_sha256) && valid_digest(shared_sha256),
                "CUDA library digests must be SHA-256 hex"
            );
            Some(CudaConfig {
                device,
                provider_sha256: provider_sha256.clone(),
                shared_sha256: shared_sha256.clone(),
            })
        }
        (None, None, None) => None,
        _ => anyhow::bail!("CUDA runtime configuration is incomplete"),
    };
    let threads = usize::from(config.inference_threads);
    let available = std::thread::available_parallelism().map_or(1, usize::from);
    ensure!(
        inference_workers == 1 || inference_workers * threads <= available,
        "configured inference workers and threads exceed available logical CPUs"
    );
    match (
        &config.model_pack,
        &config.model_manifest_sha256,
        &config.ort_library,
        &config.ort_library_sha256,
    ) {
        (Some(models), Some(manifest_sha256), Some(library), Some(library_sha256)) => {
            ensure!(
                [manifest_sha256, library_sha256]
                    .into_iter()
                    .all(|digest| valid_digest(digest)),
                "model and runtime digests must be SHA-256 hex"
            );
            Ok(Some(InferenceConfig {
                models: std::path::absolute(models)?,
                manifest_sha256: manifest_sha256.clone(),
                library: std::path::absolute(library)?,
                library_sha256: library_sha256.clone(),
                threads,
                cuda,
            }))
        }
        (None, None, None, None) => Ok(None),
        _ => anyhow::bail!("model pack configuration is incomplete"),
    }
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(target_os = "linux")]
fn start_watchdog(store: Store) -> Option<tokio::task::JoinHandle<()>> {
    let cadence = (sd_notify::watchdog_enabled()? / 2).max(Duration::from_millis(100));
    Some(tokio::spawn(async move {
        let mut ticks = tokio::time::interval(cadence);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            if matches!(
                tokio::time::timeout(cadence, store.healthy()).await,
                Ok(Ok(()))
            ) && sd_notify::notify(&[sd_notify::NotifyState::Watchdog]).is_err()
            {
                break;
            }
        }
    }))
}
