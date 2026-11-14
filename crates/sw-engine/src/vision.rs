use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use sw_domain::{ObservationLabel, SourceScope};
use sw_protocol::{
    AnalysisCadence, AnalysisMetrics, AnalysisState, AnalysisStatus, CameraPriority,
    CameraSampling, Counter, InferenceBackend, InferenceCounts, InferenceFailure, InferenceStatus,
    worker::{InferenceConfig, InferenceResult, Tasks},
};
use sw_storage::Store;
use tokio::sync::watch;
use uuid::Uuid;

use crate::{
    CameraFrame, CameraRegistry, MICROS_PER_SECOND, WorkerBinary, WorkerError,
    evidence::EvidenceSampler,
    hazards::{HazardPolicies, PendingHazards},
    inference_process::{CompletedInference, InferenceProcess},
    telemetry::{CameraMetrics, Outcome, record_dispatch, record_outcome},
};

const FRAME_MAX_AGE: Duration = Duration::from_secs(3);
const CADENCE_WINDOW_US: i64 = 10_000_000;
const SOURCE_MINUTE_US: i64 = 60_000_000;
const FIRE_BURST_FPS: u8 = 3;
const PRIORITY_BONUS: Duration = Duration::from_millis(150);
const FIRE_BURST_TIME: Duration = Duration::from_secs(2);
const FIRE_BURST_COOLDOWN: Duration = Duration::from_secs(10);

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
    metrics: HashMap<Uuid, CameraMetrics>,
    totals: InferenceCounts,
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
    burst: Option<Instant>,
    extra_fire_frames: u64,
    pose_frames: u64,
    fire_frames: u64,
    first_source_us: Option<i64>,
    pose_window: VecDeque<i64>,
    fire_window: VecDeque<i64>,
    last_at: Instant,
    queue_ms: u32,
    worker_ms: u32,
    people: Option<u16>,
    smoke: Option<u16>,
    fire: Option<u16>,
}

struct Dispatch {
    frame: Arc<CameraFrame>,
    tasks: Tasks,
    source_us: i64,
    extra_fire: bool,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum DispatchRank {
    Burst,
    Due(Duration),
    LateFire(Duration),
}

#[derive(Clone, Copy)]
struct Job {
    tasks: Tasks,
    rank: DispatchRank,
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
                metrics: HashMap::new(),
                totals: InferenceCounts::default(),
            }),
        })
    }

    pub fn inference_status(&self, include_totals: bool) -> InferenceStatus {
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
            totals: None,
        };
        if let Ok(progress) = self.progress.read() {
            if include_totals {
                result.totals = Some(progress.totals.clone());
            }
            result.ready_workers = progress
                .lanes
                .iter()
                .filter(|lane| lane.state == AnalysisState::Ready)
                .count();
            result.failure = progress
                .lanes
                .iter()
                .filter_map(|lane| lane.failure)
                .min_by_key(|failure| *failure != InferenceFailure::ModelPackUnavailable);
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

    pub fn metrics(&self, camera: Uuid) -> Option<AnalysisMetrics> {
        self.cameras.inference_lane(camera)?;
        let progress = self.progress.read().ok()?;
        Some(progress.metrics.get(&camera)?.report.clone())
    }

    pub fn status(&self, camera: Uuid, epoch: u64) -> AnalysisStatus {
        let mut response = AnalysisStatus::with_state(AnalysisState::Degraded);
        let latest = self
            .cameras
            .latest(camera)
            .filter(|frame| frame.scope.epoch == epoch);
        let source_unusable = latest
            .as_ref()
            .is_some_and(|frame| !frame.usable_for_analysis);
        if let Some(lane) = self.cameras.inference_lane(camera)
            && let Ok(progress) = self.progress.read()
        {
            let state = progress.lanes[lane].state;
            response.state = state;
            response.wall_cadence = progress
                .metrics
                .get(&camera)
                .and_then(|metrics| metrics.wall.cadence(Instant::now()));
            if state == AnalysisState::Ready && source_unusable {
                response.state = AnalysisState::Degraded;
                return response;
            }
            if let Some(analysis) = progress
                .per_camera
                .get(&camera)
                .filter(|state| state.scope.epoch == epoch)
            {
                let age = analysis.last_at.elapsed();
                response.source_epoch = Some(Counter(epoch));
                response.pose_frames = Counter(analysis.pose_frames);
                response.fire_frames = Counter(analysis.fire_frames);
                response.last_pose_source_us = analysis.pose_time.map(|pts| pts.to_string());
                response.last_fire_source_us = analysis.fire_time.map(|pts| pts.to_string());
                response.last_analysis_age_ms =
                    Some(Counter(u64::try_from(age.as_millis()).unwrap_or(u64::MAX)));
                response.last_queue_age_ms = Some(analysis.queue_ms);
                response.extra_fire_frames = Some(Counter(analysis.extra_fire_frames));
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
                if response.state == AnalysisState::Ready {
                    response.cadence = latest
                        .filter(|frame| {
                            frame.scope == analysis.scope
                                && frame.captured_at.elapsed() <= FRAME_MAX_AGE
                        })
                        .and_then(|frame| {
                            analysis.cadence(frame.frame.header.source_micros?, frame.sampling)
                        });
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
                        if let Some(commit) = pending.flush().await
                            && let Some(policy) = policies.get_mut(&commit.scope.camera_id)
                        {
                            policy.packet_committed(commit.scope, commit.incident, commit.result);
                        }
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
                        let Some(job) = self.next(lane, last_camera) else {
                            tokio::select! {
                                _ = tokio::time::sleep(Duration::from_millis(25)) => continue,
                                _ = shutdown.wait_for(|stop| *stop) => break None,
                            }
                        };
                        let dispatched_at = Instant::now();
                        self.record_dispatch(&job, dispatched_at);
                        last_camera = Some(job.frame.scope.camera_id);
                        let outcome = tokio::select! {
                            _ = shutdown.wait_for(|stop| *stop) => {
                                self.finish(&job, dispatched_at, Err(Outcome::Cancelled));
                                break None;
                            }
                            outcome = process.infer(&job.frame.frame, job.tasks) => outcome,
                        };
                        match outcome {
                            Ok(result) => {
                                if !self.finish(&job, dispatched_at, Ok(&result)) {
                                    continue;
                                }
                                let Dispatch {
                                    frame, source_us, ..
                                } = job;
                                let result = result.result;
                                policies
                                    .retain(|id, _| self.cameras.inference_lane(*id) == Some(lane));
                                let policy = policies
                                    .entry(frame.scope.camera_id)
                                    .or_insert_with(|| HazardPolicies::new(frame.scope));
                                let fire_executed = result.fire.is_some();
                                let hazards = policy.update(&frame, source_us, result);
                                if fire_executed && frame.live {
                                    self.update_burst(
                                        frame.scope,
                                        hazards.fire_candidate,
                                        dispatched_at,
                                    );
                                }
                                pending.push(frame, hazards);
                            }
                            Err(error) => {
                                let outcome = if matches!(error, WorkerError::Deadline) {
                                    Outcome::Timeout
                                } else {
                                    Outcome::WorkerFailed
                                };
                                self.finish(&job, dispatched_at, Err(outcome));
                                break Some(inference_failure(&error));
                            }
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

    fn next(&self, lane: usize, last_camera: Option<Uuid>) -> Option<Dispatch> {
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
        let now = Instant::now();
        let mut selected = None;
        for frame in frames[start..].iter().chain(&frames[..start]) {
            if frame.inference_lane != lane
                || !frame.usable_for_analysis
                || now.duration_since(frame.captured_at) > FRAME_MAX_AGE
            {
                continue;
            }
            let Some(source_us) = frame.frame.header.source_micros else {
                continue;
            };
            let previous = progress
                .per_camera
                .get(&frame.scope.camera_id)
                .filter(|state| state.scope == frame.scope);
            let Some(job) = next_job(previous, source_us, now, frame.sampling, frame.priority)
            else {
                continue;
            };
            if selected
                .as_ref()
                .is_none_or(|(rank, _, _, _)| job.rank > *rank)
            {
                selected = Some((job.rank, frame, job.tasks, source_us));
            }
        }
        selected.map(|(rank, frame, tasks, source_us)| Dispatch {
            frame: frame.clone(),
            tasks,
            source_us,
            extra_fire: rank == DispatchRank::Burst,
        })
    }

    fn record_dispatch(&self, job: &Dispatch, at: Instant) {
        let scope = job.frame.scope;
        if let Ok(mut progress) = self.progress.write() {
            if !progress.metrics.contains_key(&scope.camera_id) {
                progress
                    .metrics
                    .retain(|id, _| self.cameras.inference_lane(*id).is_some());
            }
            progress
                .metrics
                .entry(scope.camera_id)
                .or_insert_with(|| CameraMetrics::new(scope))
                .dispatch(scope, job.tasks, at - job.frame.captured_at, at);
            record_dispatch(&mut progress.totals, job.tasks);
        }
    }

    fn finish(
        &self,
        job: &Dispatch,
        dispatched_at: Instant,
        result: Result<&CompletedInference, Outcome>,
    ) -> bool {
        let received_at = Instant::now();
        let frame = &job.frame;
        let id = frame.scope.camera_id;
        let checked = result.and_then(|result| {
            if received_at - frame.captured_at > FRAME_MAX_AGE {
                return Err(Outcome::Stale);
            }
            let latest = self.cameras.latest(id).ok_or(Outcome::Scope)?;
            if latest.scope != frame.scope || !latest.usable_for_analysis {
                return Err(Outcome::Scope);
            }
            if latest.frame.generation != frame.frame.generation {
                return Err(Outcome::Generation);
            }
            Ok(result)
        });
        let Ok(mut progress) = self.progress.write() else {
            return false;
        };
        let Some(metrics) = progress.metrics.get_mut(&id) else {
            record_outcome(&mut progress.totals, job.tasks, Outcome::Scope);
            return false;
        };
        if metrics.scope() != frame.scope {
            record_outcome(&mut progress.totals, job.tasks, Outcome::Scope);
            return false;
        }
        if let Ok(result) = result {
            metrics.worker_reply(received_at - dispatched_at, result.worker_timing, job.tasks);
        }
        let accepted_at = Instant::now();
        let age = accepted_at - frame.captured_at;
        let checked = checked.and_then(|result| {
            if age > FRAME_MAX_AGE {
                Err(Outcome::Stale)
            } else {
                Ok(result)
            }
        });
        let outcome = match checked {
            Ok(_) => metrics.accept(
                job.tasks,
                frame.sampling,
                job.source_us,
                age,
                accepted_at,
                job.extra_fire,
            ),
            Err(outcome) => outcome,
        };
        record_outcome(&mut metrics.report.counts, job.tasks, outcome);
        record_outcome(&mut progress.totals, job.tasks, outcome);
        if let Ok(result) = checked {
            let current = progress
                .per_camera
                .entry(id)
                .or_insert_with(|| Analysis::new(frame.scope));
            if current.scope != frame.scope {
                *current = Analysis::new(frame.scope);
            }
            current.update(job, &result.result, dispatched_at);
        }
        checked.is_ok()
    }

    fn update_burst(&self, scope: SourceScope, candidate: bool, at: Instant) {
        if let Ok(mut progress) = self.progress.write()
            && let Some(analysis) = progress.per_camera.get_mut(&scope.camera_id)
            && analysis.scope == scope
        {
            analysis.update_burst(candidate, at);
        }
    }

    fn phase(&self, lane: usize, state: AnalysisState, failure: Option<InferenceFailure>) {
        if let Ok(mut progress) = self.progress.write() {
            if progress.lanes[lane].state != state && state == AnalysisState::Starting {
                for (camera, analysis) in &mut progress.per_camera {
                    if self.cameras.inference_lane(*camera) == Some(lane) {
                        analysis.pose_at = None;
                        analysis.fire_at = None;
                        analysis.burst = None;
                        analysis.first_source_us = None;
                        analysis.pose_window.clear();
                        analysis.fire_window.clear();
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
            burst: None,
            extra_fire_frames: 0,
            pose_frames: 0,
            fire_frames: 0,
            first_source_us: None,
            pose_window: VecDeque::new(),
            fire_window: VecDeque::new(),
            last_at: Instant::now(),
            queue_ms: 0,
            worker_ms: 0,
            people: None,
            smoke: None,
            fire: None,
        }
    }

    fn update_burst(&mut self, candidate: bool, now: Instant) {
        if candidate
            && self
                .burst
                .is_none_or(|at| now.duration_since(at) >= FIRE_BURST_COOLDOWN)
        {
            self.burst = Some(now);
        }
    }

    fn update(&mut self, job: &Dispatch, result: &InferenceResult, dispatched_at: Instant) {
        let source_us = job.source_us;
        let cutoff = source_us.saturating_sub(CADENCE_WINDOW_US);
        self.first_source_us.get_or_insert(source_us);
        for window in [&mut self.pose_window, &mut self.fire_window] {
            while window.front().is_some_and(|&sample| sample <= cutoff) {
                window.pop_front();
            }
        }
        let captured_at = job.frame.captured_at;
        if let Some(people) = &result.pose {
            self.pose_frames = self.pose_frames.saturating_add(1);
            self.pose_time = Some(source_us);
            self.pose_window.push_back(source_us);
            self.pose_at = Some(captured_at);
            self.people = Some(people.len() as u16);
        }
        if let Some(regions) = &result.fire {
            let smoke = regions
                .iter()
                .filter(|item| item.label == ObservationLabel::Smoke)
                .count();
            self.fire_frames = self.fire_frames.saturating_add(1);
            self.fire_time = Some(source_us);
            self.fire_window.push_back(source_us);
            self.fire_at = Some(captured_at);
            if job.extra_fire {
                self.extra_fire_frames = self.extra_fire_frames.saturating_add(1);
            }
            self.smoke = Some(smoke as u16);
            self.fire = Some((regions.len() - smoke) as u16);
        }
        self.last_at = captured_at;
        self.queue_ms = dispatched_at.duration_since(captured_at).as_millis() as u32;
        self.worker_ms = result.elapsed_ms;
    }

    fn cadence(&self, now: i64, sampling: CameraSampling) -> Option<AnalysisCadence> {
        if now.saturating_sub(self.first_source_us?) < CADENCE_WINDOW_US
            || self.pose_time.is_some_and(|sample| sample > now)
            || self.fire_time.is_some_and(|sample| sample > now)
        {
            return None;
        }
        let cutoff = now.saturating_sub(CADENCE_WINDOW_US);
        let per_minute = |samples: &VecDeque<i64>| {
            let count = samples
                .iter()
                .rev()
                .take_while(|&&sample| sample > cutoff)
                .count();
            count as u32 * (SOURCE_MINUTE_US / CADENCE_WINDOW_US) as u32
        };
        let pose = per_minute(&self.pose_window);
        let fire = per_minute(&self.fire_window);
        // Source frames do not always fall exactly on the requested intervals.
        let pose_target = u32::from(sampling.pose_fps) * 48;
        let fire_target = u32::from(sampling.fire_fps) * 45;
        Some(AnalysisCadence {
            pose_frames_per_minute: pose,
            fire_frames_per_minute: fire,
            pose_target_per_minute: pose_target,
            fire_target_per_minute: fire_target,
            below_target: pose < pose_target || fire < fire_target,
        })
    }
}

fn next_job(
    previous: Option<&Analysis>,
    source_us: i64,
    now: Instant,
    sampling: CameraSampling,
    priority: CameraPriority,
) -> Option<Job> {
    let fire = due(
        previous.and_then(|state| state.fire_time),
        source_us,
        sampling.fire_fps,
    );
    let pose = due(
        previous.and_then(|state| state.pose_time),
        source_us,
        sampling.pose_fps,
    );
    let pose_age = age(previous.and_then(|state| state.pose_at), now);
    let fire_age = age(previous.and_then(|state| state.fire_at), now);
    // Protect due fire work that has missed the configured operating target.
    // Eligibility is based on this camera's source clock. Ordering is monotonic.
    if fire && fire_age >= Duration::from_micros(1_333_333 / u64::from(sampling.fire_fps)) {
        return Some(Job {
            tasks: if pose { Tasks::Both } else { Tasks::Fire },
            rank: DispatchRank::LateFire(fire_age),
        });
    }
    let (tasks, age) = match (pose, fire) {
        (true, true) => (Tasks::Both, pose_age.max(fire_age)),
        (true, false) => (Tasks::Pose, pose_age),
        (false, true) => (Tasks::Fire, fire_age),
        (false, false) => {
            let state = previous?;
            return (state
                .burst
                .is_some_and(|at| now.duration_since(at) < FIRE_BURST_TIME)
                && due(state.fire_time, source_us, FIRE_BURST_FPS))
            .then_some(Job {
                tasks: Tasks::Fire,
                rank: DispatchRank::Burst,
            });
        }
    };
    let bonus = if priority == CameraPriority::High {
        PRIORITY_BONUS
    } else {
        Duration::ZERO
    };
    Some(Job {
        tasks,
        rank: DispatchRank::Due(age + bonus),
    })
}

fn age(at: Option<Instant>, now: Instant) -> Duration {
    at.map_or(FRAME_MAX_AGE, |at| now.duration_since(at))
}

fn due(previous: Option<i64>, source_us: i64, fps: u8) -> bool {
    let interval = MICROS_PER_SECOND / i64::from(fps);
    previous.is_none_or(|old| source_us.saturating_sub(old) >= interval)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analysis(at: Instant) -> Analysis {
        let mut state = Analysis::new(SourceScope {
            camera_id: Uuid::nil(),
            epoch: 1,
            scene_revision: 1,
        });
        state.pose_time = Some(1_000_000);
        state.fire_time = Some(1_000_000);
        state.pose_at = Some(at);
        state.fire_at = Some(at);
        state
    }

    #[test]
    fn camera_priority_has_a_bounded_head_start() {
        let now = Instant::now();
        let sampling = CameraSampling::default();
        let high = analysis(now - Duration::from_millis(220));
        let normal = analysis(now - Duration::from_millis(330));
        let older = analysis(now - Duration::from_millis(400));
        let rank = |camera: &Analysis, priority| {
            let job = next_job(Some(camera), 1_300_000, now, sampling, priority).unwrap();
            assert_eq!(job.tasks, Tasks::Pose);
            job.rank
        };
        assert!(rank(&high, CameraPriority::High) > rank(&normal, CameraPriority::Normal));
        assert!(rank(&older, CameraPriority::Normal) > rank(&high, CameraPriority::High));
    }

    #[test]
    fn late_fire_precedes_priority_and_idle_fire_bursts() {
        let now = Instant::now();
        let mut state = analysis(now - Duration::from_millis(800));
        state.pose_time = Some(1_750_000);
        let job = next_job(
            Some(&state),
            1_800_000,
            now,
            CameraSampling::default(),
            CameraPriority::Normal,
        )
        .unwrap();
        assert_eq!(job.tasks, Tasks::Fire);
        assert!(job.rank > DispatchRank::Due(Duration::MAX));
        assert!(job.rank > DispatchRank::Burst);
    }

    #[test]
    fn fire_burst_preserves_source_intervals_and_has_a_cooldown() {
        let now = Instant::now();
        let sampling = CameraSampling::default();
        let mut state = analysis(now);
        state.pose_time = Some(1_350_000);
        let select = |state: &Analysis, source, at| {
            next_job(Some(state), source, at, sampling, CameraPriority::Normal)
        };
        assert!(select(&state, 1_400_000, now).is_none());
        state.update_burst(true, now);
        assert!(select(&state, 1_300_000, now).is_none());
        let job = select(&state, 1_400_000, now + Duration::from_millis(400)).unwrap();
        assert_eq!(job.tasks, Tasks::Fire);
        assert_eq!(job.rank, DispatchRank::Burst);
        state.update_burst(true, now + Duration::from_secs(1));
        assert!(select(&state, 1_400_000, now + FIRE_BURST_TIME).is_none());
        state.update_burst(true, now + FIRE_BURST_COOLDOWN);
        assert_eq!(state.burst, Some(now + FIRE_BURST_COOLDOWN));
    }
}
