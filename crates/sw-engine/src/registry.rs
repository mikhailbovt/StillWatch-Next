use std::{
    collections::{BTreeMap, HashMap, HashSet},
    hash::{BuildHasher, RandomState},
    net::SocketAddr,
    path::{Component, Path, PathBuf},
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use sw_domain::{SourceScope, UnixMillis, Visibility};
use sw_protocol::{
    Camera, CameraRuntime, CameraSource, CaptureState, ControlledFireRegion, Counter, Dimensions,
    MaskRect, OnvifDevice, OnvifProfile, OnvifProfilesRequest, OnvifStream, OnvifStreamRequest,
    SceneReview, SourceProbe,
    worker::{CameraCredentials, CaptureRequest, CaptureSource, FrameHeader, PtsQuality},
};
use sw_storage::{
    CameraAction, CameraCommand, CameraLimits, CameraRecord, CameraResult, Store, StoreError,
};
use thiserror::Error;
use tokio::{
    io::AsyncReadExt,
    sync::{Semaphore, watch},
    task::JoinHandle,
};
use uuid::Uuid;

use crate::{
    CaptureProcess, CaptureRead, Frame, FrameBudget, WorkerBinary, WorkerError,
    onvif::{Onvif, OnvifConfig, OnvifError},
    replay_upload::ReplayUploads,
    scene,
};

const FRAME_TIMEOUT: Duration = Duration::from_secs(6);
const CONTENT_SAMPLE_INTERVAL: Duration = Duration::from_millis(650);
const CONTENT_HOLD_AFTER: Duration = Duration::from_secs(6);

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("camera or source configuration is invalid")]
    Invalid,
    #[error("source is unavailable or outside the configured capture scope")]
    Source,
    #[error("capture worker is unavailable")]
    WorkerUnavailable,
    #[error("capture resources are temporarily exhausted")]
    Busy,
    #[error("replay upload exceeds the configured storage budget")]
    UploadQuota,
    #[error("replay upload storage is unavailable")]
    UploadStorage(#[source] std::io::Error),
    #[error(transparent)]
    Onvif(#[from] OnvifError),
    #[error(transparent)]
    Worker(#[from] WorkerError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

pub struct CameraFrame {
    pub scope: SourceScope,
    pub captured_at: Instant,
    pub usable_for_analysis: bool,
    pub live: bool,
    pub(crate) inference_lane: usize,
    pub(crate) visibility: Visibility,
    pub(crate) controlled: Arc<[ControlledFireRegion]>,
    pub frame: Frame,
}

#[derive(Clone)]
struct State {
    phase: CaptureState,
    dropped: u64,
    last: Option<(u64, Instant)>,
    repeated_frame: bool,
    retrying: bool,
    clock_held: bool,
}

struct Slot {
    revision: u64,
    inference_lane: Option<usize>,
    current: watch::Receiver<State>,
    latest: watch::Receiver<Option<Arc<CameraFrame>>>,
}

pub struct CameraRegistry {
    store: Store,
    worker: Option<WorkerBinary>,
    replay_root: PathBuf,
    rtsp_peers: HashSet<SocketAddr>,
    credentials_root: Option<PathBuf>,
    limits: CameraLimits,
    pub(crate) inference_lanes: usize,
    budget: FrameBudget,
    probes: Semaphore,
    pub(crate) uploads: ReplayUploads,
    rtsp_confined: tokio::sync::OnceCell<bool>,
    onvif: Onvif,
    slots: RwLock<HashMap<Uuid, Slot>>,
}

pub struct SourceConfig {
    pub replay_root: PathBuf,
    pub rtsp_peers: HashSet<SocketAddr>,
    pub credentials_root: Option<PathBuf>,
    pub onvif: OnvifConfig,
}

pub struct CameraHealth {
    pub id: Uuid,
    pub revision: u64,
    pub phase: CaptureState,
    pub retrying: bool,
    pub repeated_frame: bool,
    pub clock_held: bool,
    pub usable_epoch: Option<u64>,
}

impl CameraRegistry {
    pub fn new(
        store: Store,
        worker: Option<WorkerBinary>,
        mut sources: SourceConfig,
        limits: CameraLimits,
        inference_lanes: usize,
    ) -> Result<Arc<Self>, RegistryError> {
        sources.replay_root = sources
            .replay_root
            .canonicalize()
            .map_err(|_| RegistryError::Source)?;
        if !sources.replay_root.is_dir()
            || !(1..=64).contains(&limits.active)
            || !(1..=8).contains(&inference_lanes)
        {
            return Err(RegistryError::Invalid);
        }
        if let Some(root) = &mut sources.credentials_root {
            *root = root.canonicalize().map_err(|_| RegistryError::Source)?;
            if !root.is_dir() {
                return Err(RegistryError::Invalid);
            }
        }
        let uploads = ReplayUploads::open(&sources.replay_root)?;
        let onvif = Onvif::new(sources.onvif)?;
        Ok(Arc::new(Self {
            store,
            worker,
            replay_root: sources.replay_root,
            rtsp_peers: sources.rtsp_peers,
            credentials_root: sources.credentials_root,
            limits,
            inference_lanes,
            budget: FrameBudget::new(limits.raw_bytes)?,
            probes: Semaphore::new(2),
            uploads,
            rtsp_confined: tokio::sync::OnceCell::new(),
            onvif,
            slots: RwLock::new(HashMap::new()),
        }))
    }

    pub fn view(&self, item: CameraRecord) -> Camera {
        let current = self.slots.read().ok().and_then(|slots| {
            slots
                .get(&item.id)
                .filter(|slot| slot.revision == item.revision)
                .map(|slot| slot.current.borrow().clone())
        });
        let state = current.unwrap_or_else(|| State {
            phase: if !item.config.enabled {
                CaptureState::Disabled
            } else if self.worker.is_some() {
                CaptureState::Starting
            } else {
                CaptureState::Unavailable
            },
            dropped: 0,
            last: None,
            repeated_frame: false,
            retrying: false,
            clock_held: false,
        });
        let (frame, last_frame_age_ms) = state.last.map_or((None, None), |(frame, time)| {
            (
                Some(Counter(frame)),
                Some(Counter(
                    u64::try_from(time.elapsed().as_millis()).unwrap_or(u64::MAX),
                )),
            )
        });
        Camera {
            id: item.id,
            revision: Counter(item.revision),
            scene_revision: Counter(item.scene_revision),
            source_epoch: Counter(item.epoch),
            live: item.config.source.is_live(),
            dimensions: item.dimensions,
            scene_review: if item.config.controlled_fire_regions.is_empty() {
                SceneReview::Unconfigured
            } else if item.reviewed_scene.is_some() {
                SceneReview::Confirmed
            } else {
                SceneReview::NeedsReview
            },
            config: item.config,
            runtime: CameraRuntime {
                state: state.phase,
                frame,
                dropped: Counter(state.dropped),
                last_frame_age_ms,
                repeated_frame: state.repeated_frame,
            },
        }
    }

    pub fn latest(&self, id: Uuid) -> Option<Arc<CameraFrame>> {
        self.slots.read().ok()?.get(&id)?.latest.borrow().clone()
    }

    pub fn capture_health(&self) -> Option<Vec<CameraHealth>> {
        let slots = self.slots.read().ok()?;
        Some(
            slots
                .iter()
                .map(|(&id, slot)| {
                    let current = slot.current.borrow().clone();
                    CameraHealth {
                        id,
                        revision: slot.revision,
                        phase: current.phase,
                        retrying: current.retrying,
                        repeated_frame: current.repeated_frame,
                        clock_held: current.clock_held,
                        usable_epoch: slot.latest.borrow().as_ref().and_then(|frame| {
                            (matches!(
                                current.phase,
                                CaptureState::Streaming | CaptureState::Replaying
                            ) && frame.usable_for_analysis
                                && frame.captured_at.elapsed() <= FRAME_TIMEOUT)
                                .then_some(frame.scope.epoch)
                        }),
                    }
                })
                .collect(),
        )
    }

    pub(crate) fn inference_lane(&self, id: Uuid) -> Option<usize> {
        self.slots.read().ok()?.get(&id)?.inference_lane
    }

    pub(crate) fn frames(&self) -> Vec<Arc<CameraFrame>> {
        self.slots
            .read()
            .map(|slots| {
                slots
                    .values()
                    .filter_map(|slot| slot.latest.borrow().clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub async fn devices(&self) -> Result<Vec<sw_protocol::CaptureDevice>, RegistryError> {
        let _slot = self.probes.try_acquire().map_err(|_| RegistryError::Busy)?;
        let worker = self
            .worker
            .as_ref()
            .ok_or(RegistryError::WorkerUnavailable)?;
        Ok(worker.devices().await?)
    }

    pub async fn discover_onvif(&self) -> Result<Vec<OnvifDevice>, RegistryError> {
        Ok(self.onvif.discover().await?)
    }

    pub async fn onvif_profiles(
        &self,
        request: OnvifProfilesRequest,
    ) -> Result<Vec<OnvifProfile>, RegistryError> {
        let device = self.onvif.allowed_device(&request.device_endpoint)?;
        let credentials = self.source_credentials(request.credential_ref).await?;
        Ok(self.onvif.profiles(&device, credentials.as_ref()).await?)
    }

    pub async fn onvif_stream(
        &self,
        request: OnvifStreamRequest,
    ) -> Result<OnvifStream, RegistryError> {
        let device = self.onvif.allowed_device(&request.device_endpoint)?;
        let credentials = self.source_credentials(request.credential_ref).await?;
        let source = self
            .onvif
            .stream(
                &device,
                credentials.as_ref(),
                request.credential_ref,
                request.media,
                &request.token,
            )
            .await?;
        let authorized = self.validate_source(&source).is_ok();
        Ok(OnvifStream { source, authorized })
    }

    pub async fn apply(
        &self,
        command: CameraCommand,
        now: UnixMillis,
    ) -> Result<CameraResult, RegistryError> {
        if let Some(saved) = self.store.cached_camera(command.clone(), now).await? {
            return Ok(saved);
        }
        let (config, previous, confirm_scene) = match &command.action {
            CameraAction::Create(config) => (
                Some(config.clone()),
                None,
                !config.controlled_fire_regions.is_empty(),
            ),
            CameraAction::Patch {
                id,
                revision,
                patch,
            } => {
                let previous = self.store.camera(*id).await?;
                if previous.revision != *revision {
                    if let Some(saved) = self.store.cached_camera(command, now).await? {
                        return Ok(saved);
                    }
                    return Err(StoreError::Revision {
                        current: previous.revision,
                    }
                    .into());
                }
                (
                    Some(patch.merge(previous.config.clone())),
                    Some(previous),
                    patch
                        .controlled_fire_regions
                        .as_ref()
                        .is_some_and(|regions| !regions.is_empty()),
                )
            }
            CameraAction::Delete { .. } => (None, None, false),
        };
        let (dimensions, reviewed_scene) = match config {
            Some(config) => {
                if !config.valid() {
                    return Err(RegistryError::Invalid);
                }
                if previous
                    .as_ref()
                    .is_none_or(|previous| previous.config.source != config.source)
                {
                    self.validate_source(&config.source)?;
                }
                let retained_dimensions = previous
                    .filter(|previous| {
                        !confirm_scene
                            && previous.config.enabled
                            && previous.config.source == config.source
                            && previous.config.privacy_masks == config.privacy_masks
                    })
                    .and_then(|previous| previous.dimensions);
                if config.enabled {
                    if let Some(dimensions) = retained_dimensions {
                        (Some(dimensions), None)
                    } else {
                        let (probe, scene) = self
                            .probe_frame(&config.source, config.privacy_masks)
                            .await?;
                        let reference = (confirm_scene && scene.visibility == Visibility::Good)
                            .then(|| scene.fingerprint.to_vec());
                        (Some(probe.dimensions), reference)
                    }
                } else {
                    (None, None)
                }
            }
            None => (None, None),
        };
        Ok(self
            .store
            .mutate_camera(command, dimensions, reviewed_scene, self.limits, now)
            .await?)
    }

    pub async fn probe(
        &self,
        source: &CameraSource,
        masks: Vec<MaskRect>,
    ) -> Result<SourceProbe, RegistryError> {
        Ok(self.probe_frame(source, masks).await?.0)
    }

    async fn probe_frame(
        &self,
        source: &CameraSource,
        masks: Vec<MaskRect>,
    ) -> Result<(SourceProbe, scene::SceneSample), RegistryError> {
        let request = self.request(source, masks).await?;
        self.probe_request(request, source.is_live()).await
    }

    pub(crate) async fn probe_request(
        &self,
        request: CaptureRequest,
        live: bool,
    ) -> Result<(SourceProbe, scene::SceneSample), RegistryError> {
        let _slot = self.probes.try_acquire().map_err(|_| RegistryError::Busy)?;
        let worker = self
            .worker
            .as_ref()
            .ok_or(RegistryError::WorkerUnavailable)?;
        let mut process = worker.capture(&request).await?;
        let result = process.next(&self.budget, FRAME_TIMEOUT).await;
        process.stop().await?;
        let frame = match result? {
            CaptureRead::Frame(frame) => frame,
            CaptureRead::Dropped(_) => return Err(RegistryError::Busy),
            CaptureRead::End => return Err(RegistryError::Source),
        };
        let header = frame.header;
        Ok((
            SourceProbe {
                dimensions: Dimensions {
                    width: header.width,
                    height: header.height,
                },
                live,
                native_pts: header.pts_quality == PtsQuality::Native,
            },
            scene::measure(&frame),
        ))
    }

    async fn request(
        &self,
        source: &CameraSource,
        masks: Vec<MaskRect>,
    ) -> Result<CaptureRequest, RegistryError> {
        self.validate_source(source)?;
        if masks.len() > 32 || !masks.iter().all(|mask| mask.valid()) {
            return Err(RegistryError::Invalid);
        }
        let source = match source {
            CameraSource::Replay { file, pace, .. } => {
                let file = tokio::fs::canonicalize(self.replay_root.join(file))
                    .await
                    .map_err(|_| RegistryError::Source)?;
                if !file.starts_with(&self.replay_root)
                    || !tokio::fs::metadata(&file)
                        .await
                        .is_ok_and(|meta| meta.is_file())
                {
                    return Err(RegistryError::Source);
                }
                CaptureSource::Replay { file, pace: *pace }
            }
            CameraSource::Rtsp {
                peer,
                stream_path,
                secure,
                transport,
                credential_ref,
            } => {
                let worker = self
                    .worker
                    .as_ref()
                    .ok_or(RegistryError::WorkerUnavailable)?;
                if !*self
                    .rtsp_confined
                    .get_or_try_init(|| worker.confined_rtsp())
                    .await?
                {
                    return Err(WorkerError::NetworkPolicyUnavailable.into());
                }
                let credentials = self.source_credentials(*credential_ref).await?;
                CaptureSource::Rtsp {
                    peer: *peer,
                    stream_path: stream_path.clone(),
                    secure: *secure,
                    transport: *transport,
                    credentials,
                }
            }
            CameraSource::Usb { device_id } => CaptureSource::Usb {
                device_id: device_id.clone(),
            },
        };
        Ok(CaptureRequest { source, masks })
    }

    fn validate_source(&self, source: &CameraSource) -> Result<(), RegistryError> {
        if !source.valid() {
            return Err(RegistryError::Invalid);
        }
        match source {
            CameraSource::Replay { file, .. } if !self.uploads.permits_source(Path::new(file)) => {
                Err(RegistryError::Source)
            }
            CameraSource::Replay { file, .. }
                if !Path::new(file)
                    .components()
                    .all(|part| matches!(part, Component::Normal(_))) =>
            {
                Err(RegistryError::Source)
            }
            CameraSource::Rtsp { peer, .. } if !self.rtsp_peers.contains(peer) => {
                Err(RegistryError::Source)
            }
            _ => Ok(()),
        }
    }

    async fn source_credentials(
        &self,
        id: Option<Uuid>,
    ) -> Result<Option<CameraCredentials>, RegistryError> {
        let Some(id) = id else { return Ok(None) };
        let root = self
            .credentials_root
            .as_ref()
            .ok_or(RegistryError::Source)?;
        let path = root.join(format!("{id}.json"));
        let meta = tokio::fs::symlink_metadata(&path)
            .await
            .map_err(|_| RegistryError::Source)?;
        if !meta.file_type().is_file() || meta.len() > 4096 {
            return Err(RegistryError::Source);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o077 != 0 {
                return Err(RegistryError::Source);
            }
        }
        let mut data = Vec::new();
        tokio::fs::File::open(path)
            .await
            .map_err(|_| RegistryError::Source)?
            .take(4097)
            .read_to_end(&mut data)
            .await
            .map_err(|_| RegistryError::Source)?;
        if data.len() > 4096 {
            return Err(RegistryError::Source);
        }
        let credentials: CameraCredentials =
            serde_json::from_slice(&data).map_err(|_| RegistryError::Source)?;
        if credentials.username.is_empty()
            || credentials.username.len() > 512
            || credentials.password.len() > 1024
            || credentials.username.chars().any(char::is_control)
            || credentials.password.chars().any(char::is_control)
        {
            return Err(RegistryError::Source);
        }
        Ok(Some(credentials))
    }

    pub async fn supervise(self: Arc<Self>, mut shutdown: watch::Receiver<bool>) {
        let mut changes = self.store.subscribe();
        let mut tasks = BTreeMap::<Uuid, Task>::new();
        while !*shutdown.borrow() {
            changes.borrow_and_update();
            if let Ok(records) = self.store.enabled_cameras().await {
                self.reconcile(&mut tasks, records).await;
            }
            tokio::select! {
                result = changes.changed() => if result.is_err() { break; },
                _ = shutdown.changed() => break,
                _ = tokio::time::sleep(Duration::from_secs(2)) => {},
            }
        }
        for task in tasks.values() {
            task.stop.send_replace(true);
        }
        for (_, task) in tasks {
            let _ = task.join.await;
        }
        if let Ok(mut slots) = self.slots.write() {
            slots.clear();
        }
    }

    async fn reconcile(
        self: &Arc<Self>,
        tasks: &mut BTreeMap<Uuid, Task>,
        records: Vec<CameraRecord>,
    ) {
        let (mut active, mut reserved) = (0, 0u64);
        let desired: BTreeMap<_, _> = records
            .into_iter()
            .map(|camera| {
                let bytes = camera
                    .dimensions
                    .map_or(u64::MAX, |d| u64::from(d.width) * u64::from(d.height) * 9);
                let admit = active < self.limits.active
                    && reserved.saturating_add(bytes) <= u64::from(self.limits.raw_bytes);
                if admit {
                    active += 1;
                    reserved += bytes;
                }
                (camera.id, (camera, admit))
            })
            .collect();
        let obsolete: Vec<_> = tasks
            .iter()
            .filter(|(id, task)| {
                !desired
                    .get(id)
                    .is_some_and(|(camera, admit)| *admit && camera.revision == task.revision)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in obsolete {
            if let Some(task) = tasks.remove(&id) {
                task.stop.send_replace(true);
                let _ = task.join.await;
            }
        }
        if let Ok(mut slots) = self.slots.write() {
            slots.retain(|id, _| tasks.contains_key(id));
        }
        let mut assigned = [0; 8];
        for task in tasks.values() {
            assigned[task.inference_lane] += 1;
        }
        for (id, (camera, admit)) in desired {
            if tasks.contains_key(&id) {
                continue;
            }
            let revision = camera.revision;
            let inference_lane = admit.then(|| {
                let lane = (0..self.inference_lanes)
                    .min_by_key(|&lane| assigned[lane])
                    .expect("a registry has at least one inference lane");
                assigned[lane] += 1;
                lane
            });
            let phase = if admit {
                CaptureState::Starting
            } else {
                CaptureState::CapacityExceeded
            };
            let (state, current) = watch::channel(State {
                phase,
                dropped: 0,
                last: None,
                repeated_frame: false,
                retrying: false,
                clock_held: false,
            });
            let (frame, latest) = watch::channel(None);
            if let Ok(mut slots) = self.slots.write() {
                slots.insert(
                    id,
                    Slot {
                        revision,
                        inference_lane,
                        current,
                        latest,
                    },
                );
            }
            let Some(inference_lane) = inference_lane else {
                continue;
            };
            let (stop, cancellation) = watch::channel(false);
            let registry = self.clone();
            let join = tokio::spawn(async move {
                registry
                    .run_camera(camera, inference_lane, state, frame, cancellation)
                    .await;
            });
            tasks.insert(
                id,
                Task {
                    revision,
                    inference_lane,
                    stop,
                    join,
                },
            );
        }
    }

    async fn run_camera(
        &self,
        mut camera: CameraRecord,
        inference_lane: usize,
        state: watch::Sender<State>,
        latest: watch::Sender<Option<Arc<CameraFrame>>>,
        mut stop: watch::Receiver<bool>,
    ) {
        let Some(worker) = &self.worker else {
            set_phase(&state, CaptureState::Unavailable);
            return;
        };
        let live = camera.config.source.is_live();
        let repeat = matches!(
            camera.config.source,
            CameraSource::Replay { repeat: true, .. }
        );
        let mut attempt = 0u32;
        while live || attempt < 5 {
            if *stop.borrow() {
                return;
            }
            set_phase(&state, CaptureState::Starting);
            let Ok(updated) = self
                .store
                .bump_camera_epoch(camera.id, camera.revision)
                .await
            else {
                set_phase(&state, CaptureState::Unavailable);
                return;
            };
            camera = updated;
            let connected_at = Instant::now();
            if let Ok(request) = self
                .request(&camera.config.source, camera.config.privacy_masks.clone())
                .await
                && let Ok(mut process) = worker.capture(&request).await
            {
                let outcome = self
                    .read_camera(
                        &mut camera,
                        inference_lane,
                        &mut process,
                        &state,
                        &latest,
                        &mut stop,
                    )
                    .await;
                if process.stop().await.is_err() {
                    set_phase(&state, CaptureState::Unavailable);
                    return;
                }
                if let Ok(phase) = outcome {
                    match phase {
                        CaptureState::Completed if repeat => {
                            latest.send_replace(None);
                            set_phase(&state, CaptureState::Starting);
                            tokio::select! {
                                _ = tokio::time::sleep(Duration::from_millis(250)) => {},
                                _ = stop.changed() => return,
                            }
                            attempt = 0;
                            continue;
                        }
                        CaptureState::Completed if live => {}
                        _ => {
                            set_phase(&state, phase);
                            return;
                        }
                    }
                }
            }
            latest.send_replace(None);
            set_phase(&state, CaptureState::Retrying);
            if connected_at.elapsed() > Duration::from_secs(30) {
                attempt = 0;
            }
            let jitter = u64::from(Uuid::new_v4().as_bytes()[0]);
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis((250u64 << attempt.min(7)).min(30_000) + jitter)) => {},
                _ = stop.changed() => return,
            }
            attempt = attempt.saturating_add(1);
        }
        set_phase(&state, CaptureState::Unavailable);
    }

    async fn read_camera(
        &self,
        camera: &mut CameraRecord,
        inference_lane: usize,
        process: &mut CaptureProcess,
        state: &watch::Sender<State>,
        latest: &watch::Sender<Option<Arc<CameraFrame>>>,
        stop: &mut watch::Receiver<bool>,
    ) -> Result<CaptureState, RegistryError> {
        let mut clock = FrameClock::default();
        let mut content = RepeatedFrames::default();
        let live = camera.config.source.is_live();
        let empty = Arc::<[ControlledFireRegion]>::from([]);
        let zones = Arc::from(camera.config.controlled_fire_regions.clone());
        let mut moved_since = None;
        loop {
            let read = tokio::select! {
                _ = stop.wait_for(|value| *value) => return Ok(CaptureState::Disabled),
                read = process.next(&self.budget, FRAME_TIMEOUT) => read?,
            };
            let frame = match read {
                CaptureRead::Frame(frame) => frame,
                CaptureRead::Dropped(_) => {
                    state.send_modify(|state| {
                        state.dropped = state.dropped.saturating_add(1);
                        state.phase = CaptureState::CapacityExceeded;
                        state.repeated_frame = false;
                    });
                    continue;
                }
                CaptureRead::End => return Ok(CaptureState::Completed),
            };
            let now = Instant::now();
            let (reset, geometry_changed, has_native_time) = clock.observe(frame.header, now);
            let sample = scene::measure(&frame);
            if reset {
                content.clear();
            }
            let was_held = content.holding;
            if live && has_native_time {
                content.observe(&frame.rgb, sample.fingerprint, now);
            }
            let usable = has_native_time && !content.holding;
            let resumed = was_held && usable;
            let moved = camera.reviewed_scene.as_ref().is_some_and(|reference| {
                usable
                    && sample.visibility == Visibility::Good
                    && scene::different(reference, &sample.fingerprint)
            });
            let confirmed_shift = if moved {
                now.duration_since(*moved_since.get_or_insert(now)) >= Duration::from_millis(750)
            } else {
                moved_since = None;
                false
            };
            if geometry_changed || confirmed_shift {
                *camera = self
                    .store
                    .invalidate_camera_scene(camera.id, camera.revision)
                    .await?;
            } else if reset || resumed {
                *camera = self
                    .store
                    .bump_camera_epoch(camera.id, camera.revision)
                    .await?;
            }
            let frame_id = frame.header.frame;
            let scope = SourceScope {
                camera_id: camera.id,
                epoch: camera.epoch,
                scene_revision: camera.scene_revision,
            };
            latest.send_replace(Some(Arc::new(CameraFrame {
                scope,
                captured_at: now,
                usable_for_analysis: usable,
                live,
                inference_lane,
                visibility: sample.visibility,
                controlled: if camera.reviewed_scene.is_some()
                    && !moved
                    && sample.visibility == Visibility::Good
                {
                    Arc::clone(&zones)
                } else {
                    Arc::clone(&empty)
                },
                frame,
            })));
            state.send_modify(|state| {
                state.phase = if usable {
                    if live {
                        CaptureState::Streaming
                    } else {
                        CaptureState::Replaying
                    }
                } else if content.holding {
                    CaptureState::Unavailable
                } else {
                    CaptureState::ClockHold
                };
                if usable {
                    state.last = Some((frame_id, now));
                }
                state.repeated_frame = content.holding;
                state.retrying = false;
                state.clock_held = clock
                    .hold_started
                    .is_some_and(|since| now.duration_since(since) >= Duration::from_secs(3));
            });
        }
    }
}

struct Task {
    revision: u64,
    inference_lane: usize,
    stop: watch::Sender<bool>,
    join: JoinHandle<()>,
}

fn set_phase(state: &watch::Sender<State>, phase: CaptureState) {
    state.send_modify(|state| {
        state.phase = phase;
        state.repeated_frame = false;
        state.clock_held = false;
        if phase == CaptureState::Retrying {
            state.retrying = true;
        }
    });
}

#[derive(Default)]
struct RepeatedFrames {
    hasher: RandomState,
    fingerprint: Option<[u8; scene::FINGERPRINT_BYTES]>,
    last_sample: Option<Instant>,
    repeat: Option<(u64, Instant)>,
    holding: bool,
}

impl RepeatedFrames {
    fn clear(&mut self) {
        self.fingerprint = None;
        self.last_sample = None;
        self.repeat = None;
        self.holding = false;
    }

    fn observe(&mut self, rgb: &[u8], fingerprint: [u8; scene::FINGERPRINT_BYTES], now: Instant) {
        if self.fingerprint.replace(fingerprint) != Some(fingerprint) {
            self.last_sample = Some(now);
            self.repeat = None;
            self.holding = false;
            return;
        }
        if self
            .last_sample
            .is_some_and(|at| now.duration_since(at) < CONTENT_SAMPLE_INTERVAL)
        {
            return;
        }
        // The cheap scene sample gates a bounded-rate hash of the decoded bytes.
        // Static-looking scenes with changing decoded pixels remain available.
        self.last_sample = Some(now);
        let hash = self.hasher.hash_one(rgb);
        let (_, since) = self
            .repeat
            .filter(|(old, _)| *old == hash)
            .unwrap_or((hash, now));
        self.repeat = Some((hash, since));
        self.holding = now.duration_since(since) >= CONTENT_HOLD_AFTER;
    }
}

#[derive(Default)]
struct FrameClock {
    geometry: Option<(u32, u32)>,
    last_pts: Option<i64>,
    hold_started: Option<Instant>,
}

impl FrameClock {
    fn observe(&mut self, header: FrameHeader, now: Instant) -> (bool, bool, bool) {
        let geometry = (header.width, header.height);
        let changed = self
            .geometry
            .replace(geometry)
            .is_some_and(|previous| previous != geometry);
        let pts = header
            .source_micros
            .filter(|_| header.pts_quality == PtsQuality::Native && !header.corrupt);
        let gap = pts
            .zip(self.last_pts)
            .is_some_and(|(new, old)| !(-1_000_000..=5_000_000).contains(&new.saturating_sub(old)));
        let advances =
            pts.is_some_and(|pts| changed || gap || self.last_pts.is_none_or(|last| pts > last));
        if !advances {
            self.hold_started.get_or_insert(now);
            if changed {
                self.last_pts = None;
            }
            return (changed, changed, false);
        }
        let resumed = self
            .hold_started
            .take()
            .is_some_and(|start| now.duration_since(start) > Duration::from_secs(3));
        self.last_pts = pts;
        (changed || gap || resumed, changed, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plateau_does_not_advance_analysis_and_resumption_rotates_scope() {
        let mut clock = FrameClock::default();
        let now = Instant::now();
        let frame = FrameHeader {
            width: 320,
            height: 240,
            frame: 1,
            raw_pts: Some(0),
            source_micros: Some(0),
            time_base_num: 1,
            time_base_den: 1_000,
            pts_quality: PtsQuality::Native,
            corrupt: false,
        };
        assert_eq!(clock.observe(frame, now), (false, false, true));
        assert_eq!(
            clock.observe(frame, now + Duration::from_millis(100)),
            (false, false, false)
        );
        let resumed = FrameHeader {
            raw_pts: Some(10),
            source_micros: Some(10_000),
            ..frame
        };
        assert_eq!(
            clock.observe(resumed, now + Duration::from_secs(4)),
            (true, false, true)
        );
        assert_eq!(
            clock.observe(
                FrameHeader {
                    width: 640,
                    ..resumed
                },
                now + Duration::from_secs(5)
            ),
            (true, true, true)
        );
    }

    #[test]
    fn prolonged_repeated_pixels_hold_until_actual_content_changes() {
        let mut tracker = RepeatedFrames::default();
        let now = Instant::now();
        let fingerprint = [12; scene::FINGERPRINT_BYTES];
        for tick in 0..=10 {
            tracker.observe(
                &[17, 41, 111],
                fingerprint,
                now + CONTENT_SAMPLE_INTERVAL * tick,
            );
            assert!(!tracker.holding);
        }
        let frozen = now + CONTENT_SAMPLE_INTERVAL * 11;
        tracker.observe(&[17, 41, 111], fingerprint, frozen);
        assert!(tracker.holding);

        tracker.observe(
            &[17, 42, 111],
            fingerprint,
            frozen + CONTENT_SAMPLE_INTERVAL,
        );
        assert!(!tracker.holding);
        tracker.observe(
            &[17, 42, 111],
            [13; scene::FINGERPRINT_BYTES],
            frozen + Duration::from_secs(9),
        );
        assert!(!tracker.holding);
    }
}
