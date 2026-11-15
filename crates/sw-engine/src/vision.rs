use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use sw_domain::{ObservationLabel, SourceScope};
use sw_protocol::{
    AnalysisCadence, AnalysisMetrics, AnalysisState, AnalysisStatus, CameraPriority,
    CameraSampling, Counter, InferenceBackend, InferenceCounts, InferenceFailure, InferenceStatus,
    worker::{InferenceConfig, Tasks},
};
use sw_storage::Store;
use tokio::sync::watch;
use uuid::Uuid;

use crate::{
    CameraFrame, CameraRegistry, WorkerBinary, WorkerError,
    evidence::EvidenceSampler,
    hazards::{HazardPolicies, PendingHazards},
    inference_process::{CompletedInference, InferenceProcess},
    task_clock::{Claim, FrameKey, TaskClock},
    telemetry::{CameraMetrics, Outcome, record_dispatch, record_misses, record_outcome},
};

const FRAME_MAX_AGE: Duration = Duration::from_secs(3);
const RETRY_RESET_WORK_TIME: Duration = Duration::from_secs(3);
const CADENCE_WINDOW_US: i64 = 10_000_000;
const SOURCE_MINUTE_US: i64 = 60_000_000;
const FIRE_BURST_FPS: u8 = 3;
const PRIORITY_BONUS: Duration = Duration::from_millis(150);
const FIRE_BURST_TIME: Duration = Duration::from_secs(2);
const FIRE_BURST_COOLDOWN: Duration = Duration::from_secs(10);
const FIRE_EXPLORATION_PERIOD: Duration = Duration::from_secs(10);
const EXTRA_DISPATCH_RESERVE: Duration = Duration::from_millis(10);
const MAX_COST_SHAPES: usize = 128;

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
    costs: HashMap<(usize, u32, u32), RequestCost>,
}

#[derive(Clone, Copy)]
struct Lane {
    state: AnalysisState,
    failure: Option<InferenceFailure>,
}

struct Analysis {
    scope: SourceScope,
    clock: [TaskClock; 2],
    pose_time: Option<i64>,
    fire_time: Option<i64>,
    pose_at: Option<Instant>,
    fire_at: Option<Instant>,
    burst: Option<Instant>,
    last_fire_claim: Option<(FrameKey, bool)>,
    last_exploration: Option<Instant>,
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
    claims: [Option<Claim>; 2],
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum DispatchRank {
    Explore,
    Burst,
    Due(Duration),
    Overdue(Duration),
}

#[derive(Clone, Copy)]
struct Job {
    tasks: Tasks,
    rank: DispatchRank,
    claims: [Option<Claim>; 2],
}

#[derive(Default)]
struct RequestCost {
    samples: [u32; 3],
    ewma: [u64; 3],
    maximum_recent: [u64; 3],
    last_observed: Option<Instant>,
}

impl RequestCost {
    fn observe(&mut self, tasks: Tasks, elapsed: Duration, at: Instant) {
        let i = task_index(tasks);
        let us = elapsed.as_micros().min(u64::MAX as u128) as u64;
        self.ewma[i] = if self.samples[i] == 0 {
            us
        } else {
            (self.ewma[i].saturating_mul(7).saturating_add(us)) / 8
        };
        self.maximum_recent[i] = (self.maximum_recent[i].saturating_mul(31) / 32).max(us);
        self.samples[i] = self.samples[i].saturating_add(1);
        self.last_observed = Some(at);
    }

    fn estimate(&self, tasks: Tasks) -> Option<Duration> {
        let i = task_index(tasks);
        if self.samples[i] >= 3 {
            Some(Duration::from_micros(
                self.maximum_recent[i].max(self.ewma[i].saturating_mul(2)),
            ))
        } else if tasks == Tasks::Both {
            Some(
                self.estimate(Tasks::Pose)?
                    .saturating_add(self.estimate(Tasks::Fire)?),
            )
        } else {
            None
        }
    }
}

fn task_index(tasks: Tasks) -> usize {
    match tasks {
        Tasks::Pose => 0,
        Tasks::Fire => 1,
        Tasks::Both => 2,
    }
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
                costs: HashMap::new(),
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
            .is_some_and(|frame| !self.accepts_frame(frame));
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
                        .as_ref()
                        .filter(|frame| {
                            frame.scope == analysis.scope
                                && frame.captured_at.elapsed() <= FRAME_MAX_AGE
                        })
                        .and_then(|frame| {
                            analysis.cadence(frame.frame.header.source_micros?, frame.sampling)
                        });
                    if latest.is_some_and(|frame| frame.source_lagging) {
                        response.state = AnalysisState::Degraded;
                    }
                }
            } else if state == AnalysisState::Ready {
                response.state = AnalysisState::Starting;
            }
        }
        response
    }

    pub async fn supervise(
        self: Arc<Self>,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), WorkerError> {
        let (stop, local_shutdown) = watch::channel(false);
        let evidence = Arc::new(EvidenceSampler::new(self.cameras.clone()));
        let mut sampler = tokio::spawn(evidence.clone().run(local_shutdown.clone()));
        let mut lanes = tokio::task::JoinSet::new();
        for lane in 0..self.lanes {
            let scheduler = self.clone();
            let evidence = evidence.clone();
            let mut shutdown = local_shutdown.clone();
            lanes.spawn(async move { scheduler.run(lane, evidence, &mut shutdown).await });
        }
        let mut sampler_joined = false;
        let mut result = loop {
            tokio::select! {
                _ = async { let _ = shutdown.wait_for(|stop| *stop).await; } => break Ok(()),
                result = lanes.join_next(), if !lanes.is_empty() => {
                    match result {
                        Some(Ok(Ok(()))) | None => {}
                        Some(Ok(Err(error))) => break Err(error),
                        Some(Err(_)) => break Err(WorkerError::Exit),
                    }
                }
                sample = &mut sampler => {
                    sampler_joined = true;
                    break Err(match sample {
                        Ok(Err(error)) => error,
                        _ => WorkerError::Exit,
                    });
                }
            }
        };
        stop.send_replace(true);
        while let Some(lane) = lanes.join_next().await {
            if result.is_ok() {
                result = lane
                    .map_err(|_| WorkerError::Exit)
                    .and_then(|result| result);
            }
        }
        if !sampler_joined && !matches!(sampler.await, Ok(Ok(()))) && result.is_ok() {
            result = Err(WorkerError::Exit);
        }
        result
    }

    async fn run(
        self: &Arc<Self>,
        lane: usize,
        evidence: Arc<EvidenceSampler>,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<(), WorkerError> {
        let Some(worker) = &self.worker else {
            return Ok(());
        };
        let mut attempt = 0u32;
        let mut last_camera = None;
        let mut policies = HashMap::<Uuid, HazardPolicies>::new();
        let mut pending =
            PendingHazards::new(self.store.clone(), evidence, &self.config.manifest_sha256);
        while !*shutdown.borrow() {
            self.phase(lane, AnalysisState::Starting, None);
            let process = tokio::select! {
                _ = shutdown.wait_for(|stop| *stop) => return Ok(()),
                process = InferenceProcess::start(worker, &self.config) => process,
            };
            let failure = match process {
                Ok(mut process) => {
                    self.phase(lane, AnalysisState::Ready, None);
                    let started = Instant::now();
                    let mut usable_work: Option<(Instant, Instant)> = None;
                    let failure = loop {
                        if *shutdown.borrow() {
                            break None;
                        }
                        if let Some(commit) = pending.flush().await
                            && let Some(policy) = policies.get_mut(&commit.scope.camera_id)
                        {
                            if let Some(packet) = commit.packet {
                                policy.packet_committed(commit.scope, commit.incident, packet);
                            }
                            if let Some((intent, outcome)) = commit.recovery {
                                policy.recovery_committed(intent, outcome);
                            }
                            if commit.resolved {
                                policy.record_resolution(commit.scope, commit.incident);
                            }
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
                                _ = self.cameras.wait_for_analysis_frame(lane) => continue,
                                _ = tokio::time::sleep(Duration::from_millis(25)) => continue,
                                _ = shutdown.wait_for(|stop| *stop) => break None,
                            }
                        };
                        let dispatched_at = Instant::now();
                        if !self.record_dispatch(&job, dispatched_at) {
                            continue;
                        }
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
                                if attempt > 0 {
                                    // Clear old failures once the replacement is sustaining useful work.
                                    let now = Instant::now();
                                    let first = usable_work
                                        .filter(|&(_, last)| now - last <= FRAME_MAX_AGE)
                                        .map_or(now, |(first, _)| first);
                                    usable_work = Some((first, now));
                                    if now - first >= RETRY_RESET_WORK_TIME {
                                        attempt = 0;
                                    }
                                }
                                let Dispatch {
                                    frame, source_us, ..
                                } = job;
                                let CompletedInference {
                                    result,
                                    fire_proposals,
                                    ..
                                } = result;
                                policies
                                    .retain(|id, _| self.cameras.inference_lane(*id) == Some(lane));
                                let policy = policies
                                    .entry(frame.scope.camera_id)
                                    .or_insert_with(|| HazardPolicies::new(frame.scope));
                                let fire_executed = result.fire.is_some();
                                if fire_executed {
                                    policy.refresh_resolutions(
                                        &self.store,
                                        frame.scope,
                                        Instant::now(),
                                    );
                                }
                                let hazards =
                                    policy.update(&frame, source_us, result, &fire_proposals);
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
                    process.stop().await?;
                    if started.elapsed() > Duration::from_secs(30) {
                        attempt = 0;
                    }
                    let Some(failure) = failure else {
                        return Ok(());
                    };
                    failure
                }
                Err(WorkerError::InferenceStartup(failure)) => {
                    // The model and runtime files are operator-managed. Leave the error
                    // visible until a restart with corrected configuration or artifacts.
                    self.phase(lane, AnalysisState::Degraded, Some(failure));
                    return Ok(());
                }
                Err(WorkerError::Unreaped) => return Err(WorkerError::Unreaped),
                Err(error) => inference_failure(&error),
            };
            self.phase(lane, AnalysisState::Degraded, Some(failure));
            let jitter = u64::from(Uuid::new_v4().as_bytes()[0]);
            let wait = Duration::from_millis(((1_000u64 << attempt.min(6)) + jitter).min(60_000));
            tokio::select! {
                _ = tokio::time::sleep(wait) => {},
                _ = shutdown.wait_for(|stop| *stop) => return Ok(()),
            }
            attempt = attempt.saturating_add(1);
        }
        Ok(())
    }

    fn accepts_frame(&self, frame: &CameraFrame) -> bool {
        frame.usable_for_analysis
            && self.config.cuda.as_ref().is_none_or(|config| {
                let header = &frame.frame.header;
                config.accepts_frame(header.width, header.height)
            })
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
        let mut next_base_us = u64::MAX;
        for frame in frames[start..].iter().chain(&frames[..start]) {
            if frame.inference_lane != lane
                || !self.accepts_frame(frame)
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
            let shape = (lane, frame.frame.header.width, frame.frame.header.height);
            let key = FrameKey {
                generation: frame.frame.generation,
                ordinal: frame.frame.header.frame,
                source_us,
            };
            let sampling = frame.sampling;
            let cost = progress.costs.get(&shape);
            if let Some(previous) = previous {
                let pose = previous.clock[0].until_base_us(key, sampling.pose_fps);
                let fire = previous.clock[1].until_base_us(key, sampling.fire_fps);
                let elapsed = now.duration_since(frame.captured_at).as_micros() as u64;
                next_base_us = next_base_us.min(pose.min(fire).saturating_sub(elapsed));
            } else {
                next_base_us = 0;
            }
            let Some(job) = next_job(
                previous,
                key,
                now,
                frame.sampling,
                frame.priority,
                frame.live,
                cost.and_then(|cost| cost.estimate(Tasks::Both)),
            ) else {
                continue;
            };
            if selected
                .as_ref()
                .is_none_or(|(rank, _, _, _)| job.rank > *rank)
            {
                selected = Some((job.rank, frame, job, cost));
            }
        }
        selected.and_then(|(rank, frame, job, cost)| {
            let extra_fire = matches!(rank, DispatchRank::Burst | DispatchRank::Explore);
            if extra_fire
                && !cost
                    .and_then(|cost| cost.estimate(Tasks::Fire))
                    .is_some_and(|estimate| {
                        estimate + EXTRA_DISPATCH_RESERVE <= Duration::from_micros(next_base_us)
                    })
            {
                return None;
            }
            Some(Dispatch {
                frame: frame.clone(),
                tasks: job.tasks,
                source_us: frame.frame.header.source_micros?,
                extra_fire,
                claims: job.claims,
            })
        })
    }

    fn record_dispatch(&self, job: &Dispatch, at: Instant) -> bool {
        let scope = job.frame.scope;
        if let Ok(mut progress) = self.progress.write() {
            let analysis = progress
                .per_camera
                .entry(scope.camera_id)
                .or_insert_with(|| Analysis::new(scope));
            if analysis.scope != scope {
                *analysis = Analysis::new(scope);
            }
            if !analysis
                .clock
                .iter()
                .zip(job.claims)
                .all(|(clock, claim)| claim.is_none_or(|claim| clock.eligible(claim.frame)))
            {
                return false;
            }
            let mut skipped = [0; 2];
            for (index, (clock, claim)) in analysis.clock.iter_mut().zip(job.claims).enumerate() {
                if let Some(claim) = claim {
                    skipped[index] = clock.claim(claim).expect("validated claim");
                }
            }
            if let Some(fire) = job.claims[1] {
                analysis.last_fire_claim = Some((fire.frame, job.extra_fire));
            }
            if !progress.metrics.contains_key(&scope.camera_id) {
                progress
                    .metrics
                    .retain(|id, _| self.cameras.inference_lane(*id).is_some());
            }
            progress
                .metrics
                .entry(scope.camera_id)
                .or_insert_with(|| CameraMetrics::new(scope))
                .dispatch(scope, job.tasks, skipped, at - job.frame.captured_at, at);
            record_dispatch(&mut progress.totals, job.tasks);
            record_misses(&mut progress.totals, job.tasks, skipped);
            return true;
        }
        false
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
        if result.is_ok() || matches!(result, Err(Outcome::Timeout)) {
            let shape = (
                frame.inference_lane,
                frame.frame.header.width,
                frame.frame.header.height,
            );
            if !progress.costs.contains_key(&shape)
                && progress.costs.len() >= MAX_COST_SHAPES
                && let Some(oldest) = progress
                    .costs
                    .iter()
                    .min_by_key(|(_, cost)| cost.last_observed)
                    .map(|(&shape, _)| shape)
            {
                progress.costs.remove(&oldest);
            }
            progress.costs.entry(shape).or_default().observe(
                job.tasks,
                received_at - dispatched_at,
                received_at,
            );
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
        let accepted = checked.is_ok();
        let mut missed = [0; 2];
        let claims_match = progress
            .per_camera
            .get_mut(&id)
            .filter(|analysis| analysis.scope == frame.scope)
            .is_some_and(|analysis| {
                if !analysis
                    .clock
                    .iter()
                    .zip(job.claims)
                    .all(|(clock, claim)| claim.is_none_or(|claim| clock.matches(claim)))
                {
                    return false;
                }
                for (index, (clock, claim)) in analysis.clock.iter_mut().zip(job.claims).enumerate()
                {
                    if let Some(claim) = claim {
                        missed[index] = clock.finish(claim, accepted).expect("matching claim");
                    }
                }
                true
            });
        let checked = if claims_match {
            checked
        } else {
            Err(Outcome::Scope)
        };
        record_misses(&mut progress.totals, job.tasks, missed);
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
        record_misses(&mut metrics.report.counts, job.tasks, missed);
        let outcome = match checked {
            Ok(_) => metrics.accept(job.tasks, age, accepted_at, job.extra_fire),
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
            current.update(job, result, dispatched_at);
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
            clock: std::array::from_fn(|_| TaskClock::default()),
            pose_time: None,
            fire_time: None,
            pose_at: None,
            fire_at: None,
            burst: None,
            last_fire_claim: None,
            last_exploration: None,
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

    fn update(&mut self, job: &Dispatch, reply: &CompletedInference, dispatched_at: Instant) {
        let result = &reply.result;
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
            if job.extra_fire || !regions.is_empty() || !reply.fire_proposals.is_empty() {
                self.last_exploration = Some(dispatched_at);
            } else {
                self.last_exploration.get_or_insert(dispatched_at);
            }
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
    frame: FrameKey,
    now: Instant,
    sampling: CameraSampling,
    priority: CameraPriority,
    live: bool,
    both_cost: Option<Duration>,
) -> Option<Job> {
    let mut slots = previous.map_or([Some(0); 2], |state| {
        [
            state.clock[0].due(frame, sampling.pose_fps),
            state.clock[1].due(frame, sampling.fire_fps),
        ]
    });
    let burst = previous
        .and_then(|state| state.burst)
        .is_some_and(|at| now.duration_since(at) < FIRE_BURST_TIME);
    let last_fire = previous
        .and_then(|state| state.last_fire_claim)
        .filter(|(last, _)| last.generation == frame.generation);
    let fire_spacing = last_fire.is_none_or(|(last, _)| {
        (i128::from(frame.source_us) - i128::from(last.source_us)) * i128::from(FIRE_BURST_FPS)
            >= 1_000_000
    });
    if !fire_spacing && (burst || last_fire.is_some_and(|(_, extra)| extra)) {
        slots[1] = None;
    }
    let [pose, fire] = slots.map(|slot| slot.is_some());
    let job = |tasks: Tasks, rank| Job {
        tasks,
        rank,
        claims: [
            tasks.pose().then_some(Claim {
                frame,
                base_slot: slots[0],
            }),
            tasks.fire().then_some(Claim {
                frame,
                base_slot: slots[1],
            }),
        ],
    };
    let pose_age = age(previous.and_then(|state| state.pose_at), now);
    let fire_age = age(previous.and_then(|state| state.fire_at), now);
    let fire_limit = Duration::from_micros(1_333_333 / u64::from(sampling.fire_fps));
    let pose_limit = Duration::from_micros(1_333_333 / u64::from(sampling.pose_fps));
    let pose_debt = pose_age.saturating_sub(pose_limit);
    let fire_debt = fire_age.saturating_sub(fire_limit);
    // Protect due fire work that has missed the configured operating target.
    // A camera with accumulated pose debt gets service after its Fire reply.
    let very_late_pose = pose && pose_age >= pose_limit.saturating_mul(2);
    let late_fire = fire && fire_age >= fire_limit;
    if late_fire && (!very_late_pose || pose_debt <= fire_debt) {
        return Some(job(Tasks::Fire, DispatchRank::Overdue(fire_debt)));
    }
    if very_late_pose {
        return Some(job(Tasks::Pose, DispatchRank::Overdue(pose_debt)));
    }
    let (tasks, age) = match (pose, fire) {
        (true, true)
            if both_cost.is_some_and(|cost| {
                cost <= pose_limit.saturating_sub(pose_age)
                    && cost <= fire_limit.saturating_sub(fire_age)
            }) =>
        {
            (Tasks::Both, pose_age.max(fire_age))
        }
        // A conservative combined estimate can be learned only after
        // isolated measurements. Retain single-task dispatch while cold.
        (true, true)
            if previous.is_some_and(|state| {
                state.clock[0].remainder(frame, sampling.pose_fps)
                    >= state.clock[1].remainder(frame, sampling.fire_fps)
            }) =>
        {
            (Tasks::Pose, pose_age)
        }
        (true, true) => (Tasks::Fire, fire_age),
        (true, false) => (Tasks::Pose, pose_age),
        (false, true) => (Tasks::Fire, fire_age),
        (false, false) => {
            let state = previous?;
            if !state.clock[1].eligible(frame) || !fire_spacing {
                return None;
            }
            let exploration = live
                && state
                    .last_exploration
                    .is_some_and(|at| now.duration_since(at) >= FIRE_EXPLORATION_PERIOD)
                && state.smoke == Some(0)
                && state.fire == Some(0);
            return match (burst, exploration) {
                (true, _) => Some(job(Tasks::Fire, DispatchRank::Burst)),
                (false, true) => Some(job(Tasks::Fire, DispatchRank::Explore)),
                _ => None,
            };
        }
    };
    let bonus = if priority == CameraPriority::High {
        PRIORITY_BONUS
    } else {
        Duration::ZERO
    };
    Some(job(tasks, DispatchRank::Due(age + bonus)))
}

fn age(at: Option<Instant>, now: Instant) -> Duration {
    at.map_or(FRAME_MAX_AGE, |at| now.duration_since(at))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(source_us: i64) -> FrameKey {
        FrameKey {
            generation: Uuid::nil(),
            ordinal: source_us.unsigned_abs(),
            source_us,
        }
    }

    fn account(clock: &mut TaskClock, source_us: i64) {
        let claim = Claim {
            frame: key(source_us),
            base_slot: Some(0),
        };
        assert_eq!(clock.claim(claim), Some(0));
        assert_eq!(clock.finish(claim, true), Some(0));
    }

    fn analysis(at: Instant) -> Analysis {
        let mut state = Analysis::new(SourceScope {
            camera_id: Uuid::nil(),
            epoch: 1,
            scene_revision: 1,
        });
        state.pose_time = Some(1_000_000);
        state.fire_time = Some(1_000_000);
        for clock in &mut state.clock {
            account(clock, 1_000_000);
        }
        state.pose_at = Some(at);
        state.fire_at = Some(at);
        state.last_fire_claim = Some((key(1_000_000), false));
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
            let job = next_job(
                Some(camera),
                key(1_300_000),
                now,
                sampling,
                priority,
                true,
                None,
            )
            .unwrap();
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
        state.pose_at = Some(now - Duration::from_millis(200));
        let job = next_job(
            Some(&state),
            key(1_800_000),
            now,
            CameraSampling::default(),
            CameraPriority::Normal,
            true,
            Some(Duration::from_millis(20)),
        )
        .unwrap();
        assert_eq!(job.tasks, Tasks::Fire);
        assert!(job.rank > DispatchRank::Due(Duration::MAX));
        assert!(job.rank > DispatchRank::Burst);
    }

    #[tokio::test(start_paused = true)]
    async fn overdue_fire_reply_surfaces_before_due_pose() {
        async fn reply(tasks: Tasks) -> Tasks {
            if tasks.pose() {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            if tasks.fire() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            tasks
        }

        let now = Instant::now();
        let mut state = analysis(now - Duration::from_millis(800));
        state.pose_at = Some(now - Duration::from_millis(250));
        let frame = key(1_800_000);
        let choose = |state: &Analysis, now| {
            next_job(
                Some(state),
                frame,
                now,
                CameraSampling::default(),
                CameraPriority::Normal,
                true,
                Some(Duration::from_millis(220)),
            )
            .unwrap()
        };
        assert!(state.clock[0].due(frame, 5).is_some());
        assert!(state.clock[1].due(frame, 2).is_some());
        let start = tokio::time::Instant::now();
        let fire = choose(&state, now);
        assert_eq!(reply(fire.tasks).await, Tasks::Fire);
        assert_eq!(start.elapsed(), Duration::from_millis(20));
        let claim = fire.claims[1].unwrap();
        assert_eq!(state.clock[1].claim(claim), Some(0));
        assert_eq!(state.clock[1].finish(claim, true), Some(0));
        state.fire_at = Some(now);
        let pose = choose(&state, now + start.elapsed());
        assert_eq!(reply(pose.tasks).await, Tasks::Pose);
        assert_eq!(start.elapsed(), Duration::from_millis(220));
    }

    #[test]
    fn scheduler_services_each_camera_and_reports_both_deficits_under_overload() {
        fn simulate(pose_ms: u64, fire_ms: u64) -> Vec<([u64; 2], [u64; 2])> {
            let start = Instant::now();
            let mut states: Vec<_> = (0..4).map(|_| analysis(start)).collect();
            let mut counts = vec![([0; 2], [0; 2]); states.len()];
            let mut time = 0;
            let mut first = 0;
            while time < 60_000 {
                let now = start + Duration::from_millis(time);
                let mut selected = None;
                for offset in 0..states.len() {
                    let i = (first + offset) % states.len();
                    let pts = 1_000_000 + ((time * 15 / 1_000) * 1_000_000).div_ceil(15);
                    let Some(job) = next_job(
                        Some(&states[i]),
                        key(pts as i64),
                        now,
                        CameraSampling::default(),
                        if i == 0 {
                            CameraPriority::High
                        } else {
                            CameraPriority::Normal
                        },
                        false,
                        Some(Duration::from_millis(2 * (pose_ms + fire_ms))),
                    ) else {
                        continue;
                    };
                    if selected.is_none_or(|(_, best): (usize, Job)| job.rank > best.rank) {
                        selected = Some((i, job));
                    }
                }
                let Some((i, job)) = selected else {
                    time += 1;
                    continue;
                };
                let state = &mut states[i];
                for (index, claim) in job.claims.into_iter().enumerate() {
                    if let Some(claim) = claim {
                        counts[i].0[index] += 1;
                        counts[i].1[index] += state.clock[index].claim(claim).unwrap();
                        state.clock[index].finish(claim, true).unwrap();
                        if index == 0 {
                            state.pose_at = Some(now);
                        } else {
                            state.fire_at = Some(now);
                        }
                    }
                }
                time += if job.tasks.pose() { pose_ms } else { 0 };
                time += if job.tasks.fire() { fire_ms } else { 0 };
                first = (i + 1) % states.len();
            }
            counts
        }
        for (accepted, missed) in simulate(7, 5) {
            assert_eq!(accepted, [299, 119]);
            assert_eq!(missed, [0, 0]);
        }
        for (accepted, missed) in simulate(200, 100) {
            assert!(accepted[0] > 0 && accepted[0] < 240, "{accepted:?}");
            assert!(accepted[1] > 0 && accepted[1] < 90, "{accepted:?}");
            assert!(missed[0] > 0 && missed[1] > 0, "{missed:?}");
        }
    }

    #[test]
    fn fire_burst_preserves_source_intervals_and_has_a_cooldown() {
        let now = Instant::now();
        let sampling = CameraSampling::default();
        let mut state = analysis(now);
        state.pose_time = Some(1_350_000);
        state.clock[0] = TaskClock::default();
        account(&mut state.clock[0], 1_350_000);
        let select = |state: &Analysis, source, at| {
            next_job(
                Some(state),
                key(source),
                at,
                sampling,
                CameraPriority::Normal,
                true,
                None,
            )
        };
        assert!(select(&state, 1_400_000, now).is_none());
        state.update_burst(true, now);
        assert!(select(&state, 1_300_000, now).is_none());
        let job = select(&state, 1_400_000, now + Duration::from_millis(400)).unwrap();
        assert_eq!(job.tasks, Tasks::Fire);
        assert_eq!(job.rank, DispatchRank::Burst);
        let claim = job.claims[1].unwrap();
        assert!(claim.base_slot.is_none());
        assert_eq!(state.clock[1].claim(claim), Some(0));
        assert_eq!(state.clock[1].finish(claim, true), Some(0));
        state.last_fire_claim = Some((claim.frame, true));
        let at = now + Duration::from_millis(500);
        assert!(!select(&state, 1_500_000, at).is_some_and(|job| job.tasks.fire()));
        assert_eq!(
            select(&state, 1_750_000, at).unwrap().claims[1]
                .unwrap()
                .base_slot,
            Some(1)
        );
        state.update_burst(true, now + Duration::from_secs(1));
        assert!(select(&state, 1_400_000, now + FIRE_BURST_TIME).is_none());
        state.update_burst(true, now + FIRE_BURST_COOLDOWN);
        assert_eq!(state.burst, Some(now + FIRE_BURST_COOLDOWN));
    }

    #[test]
    fn zero_proposal_scan_uses_only_spare_live_slots() {
        let now = Instant::now();
        let mut state = analysis(now);
        state.pose_time = Some(1_350_000);
        state.clock[0] = TaskClock::default();
        account(&mut state.clock[0], 1_350_000);
        state.smoke = Some(0);
        state.fire = Some(0);
        state.last_exploration = Some(now - FIRE_EXPLORATION_PERIOD);
        let choose = |source, live| {
            next_job(
                Some(&state),
                key(source),
                now,
                CameraSampling::default(),
                CameraPriority::Normal,
                live,
                None,
            )
        };
        assert!(choose(1_400_000, false).is_none());
        let idle = choose(1_400_000, true).unwrap();
        assert_eq!(idle.rank, DispatchRank::Explore);
        let base = choose(1_650_000, true).unwrap();
        assert!(base.rank > idle.rank && base.tasks.fire());
    }
}
