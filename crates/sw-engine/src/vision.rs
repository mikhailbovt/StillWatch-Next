use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use sw_domain::{ObservationLabel, SourceScope};
use sw_protocol::{
    AnalysisState, AnalysisStatus, Counter, InferenceBackend, InferenceFailure, InferenceStatus,
    worker::{InferenceConfig, InferenceResult, Tasks},
};
use sw_storage::Store;
use tokio::sync::watch;
use uuid::Uuid;

use crate::{
    CameraFrame, CameraRegistry, WorkerBinary, WorkerError,
    evidence::EvidenceSampler,
    hazards::{HazardPolicies, PendingHazards},
    inference_process::InferenceProcess,
};

const POSE_INTERVAL_US: i64 = 200_000;
const FIRE_INTERVAL_US: i64 = 500_000;
const FRAME_MAX_AGE: Duration = Duration::from_secs(3);

pub struct VisionScheduler {
    config: InferenceConfig,
    worker: Option<WorkerBinary>,
    cameras: Arc<CameraRegistry>,
    store: Store,
    lanes: usize,
    progress: RwLock<Progress>,
}

struct Progress {
    lanes: Vec<Lane>,
    per_camera: HashMap<Uuid, Analysis>,
}

#[derive(Clone, Copy)]
struct Lane {
    state: AnalysisState,
    failure: Option<InferenceFailure>,
}

struct Analysis {
    scope: SourceScope,
    pose_time: Option<i64>,
    fire_time: Option<i64>,
    pose_at: Option<Instant>,
    fire_at: Option<Instant>,
    pose_frames: u64,
    fire_frames: u64,
    last_at: Instant,
    worker_ms: u32,
    people: Option<u16>,
    smoke: Option<u16>,
    fire: Option<u16>,
}

impl VisionScheduler {
    pub fn new(
        worker: Option<WorkerBinary>,
        config: InferenceConfig,
        cameras: Arc<CameraRegistry>,
        store: Store,
    ) -> Arc<Self> {
        let lanes = cameras.inference_lanes;
        let initial = if worker.is_some() {
            Lane {
                state: AnalysisState::Starting,
                failure: None,
            }
        } else {
            Lane {
                state: AnalysisState::Degraded,
                failure: Some(InferenceFailure::WorkerUnavailable),
            }
        };
        Arc::new(Self {
            config,
            worker,
            cameras,
            store,
            lanes,
            progress: RwLock::new(Progress {
                lanes: vec![initial; lanes],
                per_camera: HashMap::new(),
            }),
        })
    }

    pub fn inference_status(&self) -> InferenceStatus {
        let mut result = InferenceStatus {
            state: AnalysisState::Starting,
            backend: Some(if self.config.cuda.is_some() {
                InferenceBackend::Cuda
            } else {
                InferenceBackend::Cpu
            }),
            ready_workers: 0,
            configured_workers: self.lanes,
            failure: None,
        };
        if let Ok(progress) = self.progress.read() {
            result.ready_workers = progress
                .lanes
                .iter()
                .filter(|lane| lane.state == AnalysisState::Ready)
                .count();
            result.failure = progress.lanes.iter().find_map(|lane| lane.failure);
            result.state = if result.ready_workers == self.lanes {
                AnalysisState::Ready
            } else if result.failure.is_some() {
                AnalysisState::Degraded
            } else {
                AnalysisState::Starting
            };
        } else {
            result.state = AnalysisState::Degraded;
            result.failure = Some(InferenceFailure::WorkerFailed);
        }
        result
    }

    pub fn status(&self, camera: Uuid, epoch: u64) -> AnalysisStatus {
        let mut response = AnalysisStatus::with_state(AnalysisState::Degraded);
        if let Some(lane) = self.cameras.inference_lane(camera)
            && let Ok(progress) = self.progress.read()
        {
            let state = progress.lanes[lane].state;
            response.state = state;
            if let Some(analysis) = progress
                .per_camera
                .get(&camera)
                .filter(|state| state.scope.epoch == epoch)
            {
                let age = analysis.last_at.elapsed();
                response.source_epoch = Some(Counter(epoch));
                response.pose_frames = Counter(analysis.pose_frames);
                response.fire_frames = Counter(analysis.fire_frames);
                response.last_analysis_age_ms =
                    Some(Counter(u64::try_from(age.as_millis()).unwrap_or(u64::MAX)));
                response.last_worker_ms = Some(analysis.worker_ms);
                let pose_fresh = analysis
                    .pose_at
                    .is_some_and(|time| time.elapsed() <= FRAME_MAX_AGE);
                let fire_fresh = analysis
                    .fire_at
                    .is_some_and(|time| time.elapsed() <= FRAME_MAX_AGE);
                if state == AnalysisState::Ready && pose_fresh {
                    response.people = analysis.people;
                }
                if state == AnalysisState::Ready && fire_fresh {
                    response.smoke_regions = analysis.smoke;
                    response.fire_regions = analysis.fire;
                }
                if state == AnalysisState::Ready && (!pose_fresh || !fire_fresh) {
                    response.state = AnalysisState::Degraded;
                }
            } else if state == AnalysisState::Ready {
                response.state = AnalysisState::Starting;
            }
        }
        response
    }

    pub async fn supervise(self: Arc<Self>, shutdown: watch::Receiver<bool>) {
        let (stop_evidence, evidence_shutdown) = watch::channel(false);
        let evidence = Arc::new(EvidenceSampler::new(self.cameras.clone()));
        let sampler = tokio::spawn(evidence.clone().run(evidence_shutdown));
        let mut lanes = tokio::task::JoinSet::new();
        let mut indices = HashMap::with_capacity(self.lanes);
        for lane in 0..self.lanes {
            let scheduler = self.clone();
            let evidence = evidence.clone();
            let mut shutdown = shutdown.clone();
            let task =
                lanes.spawn(async move { scheduler.run(lane, evidence, &mut shutdown).await });
            indices.insert(task.id(), lane);
        }
        while let Some(result) = lanes.join_next_with_id().await {
            if let Err(error) = result {
                if let Some(&lane) = indices.get(&error.id()) {
                    self.phase(
                        lane,
                        AnalysisState::Degraded,
                        Some(InferenceFailure::WorkerFailed),
                    );
                }
                eprintln!("StillWatch inference lane terminated unexpectedly");
            }
        }
        stop_evidence.send_replace(true);
        let _ = sampler.await;
    }

    async fn run(
        self: &Arc<Self>,
        lane: usize,
        evidence: Arc<EvidenceSampler>,
        shutdown: &mut watch::Receiver<bool>,
    ) {
        let Some(worker) = &self.worker else {
            return;
        };
        let mut attempt = 0u32;
        let mut last_camera = None;
        let mut policies = HashMap::<Uuid, HazardPolicies>::new();
        let mut pending =
            PendingHazards::new(self.store.clone(), evidence, &self.config.manifest_sha256);
        while !*shutdown.borrow() {
            self.phase(lane, AnalysisState::Starting, None);
            let process = tokio::select! {
                _ = shutdown.wait_for(|stop| *stop) => return,
                process = InferenceProcess::start(worker, &self.config) => process,
            };
            let failure = match process {
                Ok(mut process) => {
                    self.phase(lane, AnalysisState::Ready, None);
                    let started = Instant::now();
                    let failure = loop {
                        if *shutdown.borrow() {
                            break None;
                        }
                        pending.flush().await;
                        if pending.blocked() {
                            self.phase(
                                lane,
                                AnalysisState::Degraded,
                                Some(InferenceFailure::EvidenceBackpressure),
                            );
                            tokio::select! {
                                _ = tokio::time::sleep(Duration::from_millis(100)) => continue,
                                _ = shutdown.wait_for(|stop| *stop) => break None,
                            }
                        }
                        self.phase(lane, AnalysisState::Ready, None);
                        let Some((frame, tasks, source_us)) = self.next(lane, last_camera) else {
                            tokio::select! {
                                _ = tokio::time::sleep(Duration::from_millis(25)) => continue,
                                _ = shutdown.wait_for(|stop| *stop) => break None,
                            }
                        };
                        last_camera = Some(frame.scope.camera_id);
                        let outcome = tokio::select! {
                            _ = shutdown.wait_for(|stop| *stop) => break None,
                            outcome = process.infer(&frame.frame, tasks) => outcome,
                        };
                        match outcome {
                            Ok(result) if self.accept(&frame, source_us, &result) => {
                                policies
                                    .retain(|id, _| self.cameras.inference_lane(*id) == Some(lane));
                                let policy = policies
                                    .entry(frame.scope.camera_id)
                                    .or_insert_with(|| HazardPolicies::new(frame.scope));
                                let hazards = policy.update(&frame, source_us, result);
                                pending.push(frame, hazards);
                            }
                            Ok(_) => {}
                            Err(error) => break Some(inference_failure(&error)),
                        }
                    };
                    if process.stop().await.is_err() {
                        self.phase(
                            lane,
                            AnalysisState::Degraded,
                            Some(InferenceFailure::WorkerFailed),
                        );
                        return;
                    }
                    if started.elapsed() > Duration::from_secs(30) {
                        attempt = 0;
                    }
                    let Some(failure) = failure else { return };
                    failure
                }
                Err(WorkerError::InferenceStartup(failure)) => {
                    // The model and runtime files are operator-managed. Leave the error
                    // visible until a restart with corrected configuration or artifacts.
                    self.phase(lane, AnalysisState::Degraded, Some(failure));
                    return;
                }
                Err(error) => inference_failure(&error),
            };
            self.phase(lane, AnalysisState::Degraded, Some(failure));
            let jitter = u64::from(Uuid::new_v4().as_bytes()[0]);
            let wait = Duration::from_millis(((1_000u64 << attempt.min(6)) + jitter).min(60_000));
            tokio::select! {
                _ = tokio::time::sleep(wait) => {},
                _ = shutdown.wait_for(|stop| *stop) => return,
            }
            attempt = attempt.saturating_add(1);
        }
    }

    fn next(
        &self,
        lane: usize,
        last_camera: Option<Uuid>,
    ) -> Option<(Arc<CameraFrame>, Tasks, i64)> {
        let mut frames = self.cameras.frames();
        frames.sort_unstable_by_key(|frame| frame.scope.camera_id);
        let mut progress = self.progress.write().ok()?;
        progress.per_camera.retain(|id, _| {
            frames
                .binary_search_by_key(id, |frame| frame.scope.camera_id)
                .is_ok()
        });
        let start = last_camera.map_or(0, |id| {
            frames.partition_point(|frame| frame.scope.camera_id <= id)
        });
        frames[start..]
            .iter()
            .chain(&frames[..start])
            .find_map(|frame| {
                if frame.inference_lane != lane
                    || !frame.usable_for_analysis
                    || frame.captured_at.elapsed() > FRAME_MAX_AGE
                {
                    return None;
                }
                let source_us = frame.frame.header.source_micros?;
                let previous = progress
                    .per_camera
                    .get(&frame.scope.camera_id)
                    .filter(|state| state.scope == frame.scope);
                let pose = due(
                    previous.and_then(|state| state.pose_time),
                    source_us,
                    POSE_INTERVAL_US,
                );
                let fire = due(
                    previous.and_then(|state| state.fire_time),
                    source_us,
                    FIRE_INTERVAL_US,
                );
                let tasks = match (pose, fire) {
                    (true, true) => Tasks::Both,
                    (true, false) => Tasks::Pose,
                    (false, true) => Tasks::Fire,
                    _ => return None,
                };
                Some((frame.clone(), tasks, source_us))
            })
    }

    fn accept(&self, frame: &CameraFrame, source_us: i64, result: &InferenceResult) -> bool {
        if frame.captured_at.elapsed() > FRAME_MAX_AGE
            || !self
                .cameras
                .latest(frame.scope.camera_id)
                .is_some_and(|newest| {
                    newest.scope == frame.scope
                        && newest.frame.generation == frame.frame.generation
                        && newest.usable_for_analysis
                })
        {
            return false;
        }
        let Ok(mut progress) = self.progress.write() else {
            return false;
        };
        let current = progress
            .per_camera
            .entry(frame.scope.camera_id)
            .or_insert_with(|| Analysis::new(frame.scope));
        if current.scope != frame.scope {
            *current = Analysis::new(frame.scope);
        }
        if let Some(people) = &result.pose {
            current.pose_frames = current.pose_frames.saturating_add(1);
            current.pose_time = Some(source_us);
            current.pose_at = Some(frame.captured_at);
            current.people = Some(people.len() as u16);
        }
        if let Some(regions) = &result.fire {
            let smoke = regions
                .iter()
                .filter(|item| item.label == ObservationLabel::Smoke)
                .count();
            current.fire_frames = current.fire_frames.saturating_add(1);
            current.fire_time = Some(source_us);
            current.fire_at = Some(frame.captured_at);
            current.smoke = Some(smoke as u16);
            current.fire = Some((regions.len() - smoke) as u16);
        }
        current.last_at = frame.captured_at;
        current.worker_ms = result.elapsed_ms;
        true
    }

    fn phase(&self, lane: usize, state: AnalysisState, failure: Option<InferenceFailure>) {
        if let Ok(mut progress) = self.progress.write() {
            if progress.lanes[lane].state != state && state == AnalysisState::Starting {
                for (camera, analysis) in &mut progress.per_camera {
                    if self.cameras.inference_lane(*camera) == Some(lane) {
                        analysis.pose_at = None;
                        analysis.fire_at = None;
                    }
                }
            }
            progress.lanes[lane] = Lane { state, failure };
        }
    }
}

fn inference_failure(error: &WorkerError) -> InferenceFailure {
    match error {
        WorkerError::Deadline => InferenceFailure::WorkerTimeout,
        WorkerError::Unavailable => InferenceFailure::WorkerUnavailable,
        _ => InferenceFailure::WorkerFailed,
    }
}

impl Analysis {
    fn new(scope: SourceScope) -> Self {
        Self {
            scope,
            pose_time: None,
            fire_time: None,
            pose_at: None,
            fire_at: None,
            pose_frames: 0,
            fire_frames: 0,
            last_at: Instant::now(),
            worker_ms: 0,
            people: None,
            smoke: None,
            fire: None,
        }
    }
}

fn due(previous: Option<i64>, source_us: i64, interval: i64) -> bool {
    previous.is_none_or(|old| source_us.saturating_sub(old) >= interval)
}
