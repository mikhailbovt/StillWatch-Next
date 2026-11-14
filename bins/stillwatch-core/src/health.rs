use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use sw_engine::{CameraHealth, CameraRegistry, SemanticSupervisor, VisionScheduler};
use sw_protocol::{AnalysisState, CaptureState, InferenceFailure};
use sw_storage::{HealthIssue, HealthObservation, Store};
use tokio::sync::watch;

pub(crate) const LOW_DISK_BYTES: u64 = 512 * 1024 * 1024;
const STALE_ANALYSIS_AFTER: Duration = Duration::from_secs(6);
const CONFIRM_CADENCE_AFTER: Duration = Duration::from_secs(6);
const MAX_CLOCK_STEP: Duration = Duration::from_secs(5);
const CLOCK_STABLE_FOR: Duration = Duration::from_secs(30);

pub(crate) async fn monitor(
    store: Store,
    registry: Arc<CameraRegistry>,
    vision: Option<Arc<VisionScheduler>>,
    semantic: Option<Arc<SemanticSupervisor>>,
    mut stop: watch::Receiver<bool>,
) {
    use HealthIssue::*;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut recorded = BTreeSet::new();
    let mut stale_analysis = HashMap::new();
    let mut cadence = HashMap::new();
    let started = Instant::now();
    let mut clock = WallClock {
        previous: (started, now_ms()),
        stable_after: started + CLOCK_STABLE_FOR,
        has_step: false,
    };
    let mut write_failed = false;
    while !*stop.borrow() {
        tokio::select! {
            _ = stop.wait_for(|stopping| *stopping) => return,
            _ = tick.tick() => {}
        }
        let (now, utc) = (Instant::now(), now_ms());
        let mut observations = BTreeSet::new();
        if let Some(unstable) = clock.observe(now, utc) {
            observations.insert(HealthObservation::global(HostClockDiscontinuity, unstable));
        }
        if let Some(vision) = &vision {
            let status = vision.inference_status(false);
            if status.state != AnalysisState::Starting {
                observations.insert(HealthObservation::global(
                    InferenceUnavailable,
                    status.state == AnalysisState::Degraded,
                ));
            }
            match status.failure {
                Some(InferenceFailure::ModelPackUnavailable) => {
                    observations.insert(HealthObservation::global(ModelPackUnverified, true));
                }
                Some(
                    InferenceFailure::RuntimeUnavailable
                    | InferenceFailure::ModelInitializationFailed,
                ) if status.configured_workers == 1 => {
                    observations.insert(HealthObservation::global(ModelPackUnverified, false));
                }
                _ if status.state == AnalysisState::Ready => {
                    observations.insert(HealthObservation::global(ModelPackUnverified, false));
                }
                _ => {}
            }
        } else {
            observations.extend([
                HealthObservation::global(InferenceUnavailable, false),
                HealthObservation::global(ModelPackUnverified, false),
            ]);
        }
        if let Some(semantic) = &semantic {
            let status = semantic.status();
            if let Some(ready) = status.route_ready {
                observations.extend([
                    HealthObservation::global(SemanticUnavailable, !ready),
                    HealthObservation::global(
                        SemanticBudgetExhausted,
                        status.used_today >= status.daily_request_limit,
                    ),
                ]);
            }
        } else {
            observations.extend([
                HealthObservation::global(SemanticUnavailable, false),
                HealthObservation::global(SemanticBudgetExhausted, false),
            ]);
        }
        if let Some(cameras) = registry.capture_health() {
            stale_analysis.retain(|id, _| cameras.iter().any(|camera| camera.id == *id));
            cadence.retain(|id, _| cameras.iter().any(|camera| camera.id == *id));
            for camera in cameras {
                let analysis = camera
                    .usable_epoch
                    .zip(vision.as_ref())
                    .map(|(epoch, vision)| (epoch, vision.status(camera.id, epoch)));
                let analysis_state = analysis
                    .as_ref()
                    .map(|(epoch, status)| (*epoch, status.state));
                let covered = camera.usable_epoch.is_some()
                    && (vision.is_none()
                        || analysis_state.is_some_and(|(_, state)| state == AnalysisState::Ready));
                observe_camera(&camera, covered, &mut observations);
                if let Some((epoch, state)) =
                    analysis_state.filter(|(_, state)| *state != AnalysisState::Ready)
                {
                    let (previous_epoch, since) = stale_analysis
                        .entry(camera.id)
                        .or_insert_with(|| (epoch, Instant::now()));
                    if *previous_epoch != epoch {
                        (*previous_epoch, *since) = (epoch, Instant::now());
                    }
                    if state == AnalysisState::Degraded || since.elapsed() >= STALE_ANALYSIS_AFTER {
                        observations.insert(for_camera(&camera, InferenceUnavailable, true));
                    }
                } else {
                    stale_analysis.remove(&camera.id);
                    if analysis.is_some()
                        || vision.is_none()
                        || camera.phase == CaptureState::Completed
                    {
                        observations.insert(for_camera(&camera, InferenceUnavailable, false));
                    }
                }
                if let Some((epoch, measured)) = analysis.and_then(|(epoch, status)| {
                    status
                        .cadence
                        .map(|measured| (epoch, measured.below_target))
                }) {
                    let reading = (epoch, measured);
                    let (previous, since) = cadence.entry(camera.id).or_insert((reading, now));
                    if *previous != reading {
                        (*previous, *since) = (reading, now);
                    }
                    if now.duration_since(*since) >= CONFIRM_CADENCE_AFTER {
                        observations.insert(for_camera(&camera, InferenceBelowTarget, measured));
                    }
                } else {
                    cadence.remove(&camera.id);
                    if vision.is_none() || camera.phase == CaptureState::Completed {
                        observations.insert(for_camera(&camera, InferenceBelowTarget, false));
                    }
                }
            }
        }
        let changed: Vec<_> = observations.difference(&recorded).copied().collect();
        if !changed.is_empty() && store.record_health(changed, utc).await.is_err() {
            if !write_failed {
                eprintln!("StillWatch could not record technical health changes");
            }
            write_failed = true;
        } else {
            write_failed = false;
            recorded = observations;
        }
    }
}

fn for_camera(camera: &CameraHealth, issue: HealthIssue, active: bool) -> HealthObservation {
    HealthObservation {
        camera: Some((camera.id, camera.revision)),
        issue,
        active,
    }
}

fn observe_camera(
    camera: &CameraHealth,
    covered: bool,
    observations: &mut BTreeSet<HealthObservation>,
) {
    use HealthIssue::*;
    let phase = camera.phase;
    let can_receive = matches!(
        phase,
        CaptureState::Streaming | CaptureState::Replaying | CaptureState::ClockHold
    ) || camera.repeated_frame;
    let capture_ended = matches!(
        phase,
        CaptureState::CapacityExceeded | CaptureState::Completed
    );
    let offline = !can_receive && (camera.retrying || phase == CaptureState::Unavailable);
    if covered || capture_ended || offline {
        observations.insert(for_camera(camera, CameraOffline, !capture_ended && offline));
    }
    if phase != CaptureState::Starting {
        observations.insert(for_camera(
            camera,
            CameraCapacityExceeded,
            phase == CaptureState::CapacityExceeded,
        ));
    }
    let capture_status_known = camera.usable_epoch.is_some() || offline || capture_ended;
    if camera.repeated_frame || capture_status_known {
        observations.insert(for_camera(camera, CameraFrozen, camera.repeated_frame));
    }
    if camera.clock_held || capture_status_known {
        observations.insert(for_camera(camera, InvalidSourceClock, camera.clock_held));
    }
}

pub(crate) async fn check_disk(store: &Store, root: &Path) -> anyhow::Result<()> {
    let root = root.to_path_buf();
    let available = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::task::spawn_blocking(move || fs4::available_space(root)),
    )
    .await??
    .ok();
    let mut observations = vec![HealthObservation::global(
        HealthIssue::DiskSpaceUnknown,
        available.is_none(),
    )];
    if let Some(bytes) = available {
        observations.push(HealthObservation::global(
            HealthIssue::DiskSpaceLow,
            bytes < LOW_DISK_BYTES,
        ));
    }
    match store.database_budget_low().await {
        Ok(low) => observations.push(HealthObservation::global(
            HealthIssue::DatabaseBudgetLow,
            low,
        )),
        Err(_) => eprintln!("StillWatch could not check the metadata storage budget"),
    }
    if store.record_health(observations, now_ms()).await.is_err() {
        eprintln!("StillWatch could not record storage health");
    }
    Ok(())
}

struct WallClock {
    previous: (Instant, i64),
    stable_after: Instant,
    has_step: bool,
}

impl WallClock {
    fn observe(&mut self, monotonic: Instant, utc: i64) -> Option<bool> {
        let (last, wall) = self.previous;
        let expected = monotonic.duration_since(last).as_millis() as i64;
        if utc.saturating_sub(wall).abs_diff(expected) > MAX_CLOCK_STEP.as_millis() as u64 {
            self.stable_after = monotonic + CLOCK_STABLE_FOR;
            self.has_step = true;
        }
        self.previous = (monotonic, utc);
        (self.has_step || monotonic >= self.stable_after).then_some(monotonic < self.stable_after)
    }
}

fn now_ms() -> i64 {
    (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_clock_intervals_require_observed_stability() {
        let began = Instant::now();
        let mut clock = WallClock {
            previous: (began, 1_000_000),
            stable_after: began + CLOCK_STABLE_FOR,
            has_step: false,
        };
        assert_eq!(
            clock.observe(began + Duration::from_secs(1), 1_001_000),
            None
        );
        assert_eq!(
            clock.observe(began + Duration::from_secs(2), 992_000),
            Some(true)
        );
        assert_eq!(
            clock.observe(began + Duration::from_secs(31), 1_021_000),
            Some(true)
        );
        assert_eq!(
            clock.observe(began + Duration::from_secs(32), 1_022_000),
            Some(false)
        );
    }
}
