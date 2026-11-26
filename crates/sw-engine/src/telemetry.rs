use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use sw_domain::SourceScope;
use sw_protocol::{
    AnalysisMetrics, Counter, InferenceCounts, LatencyDistribution, TaskDurations, WallCadence,
    WorkerDurations,
    worker::{ExecutionTiming, Tasks},
};

const LATENCY_BOUNDS_US: [u32; 25] = [
    25, 50, 100, 200, 500, 1_000, 2_000, 4_000, 8_000, 16_000, 24_000, 32_000, 50_000, 75_000,
    100_000, 150_000, 200_000, 250_000, 350_000, 500_000, 750_000, 1_000_000, 1_500_000, 2_000_000,
    3_000_000,
];
const WALL_WINDOW: Duration = Duration::from_secs(60);
const MAX_WALL_SAMPLES: usize = 1024;

#[derive(Clone, Copy)]
pub(crate) enum Outcome {
    Accepted { burst: bool },
    Timeout,
    WorkerFailed,
    Cancelled,
    Stale,
    Scope,
    Generation,
}

impl Outcome {
    pub fn name(self) -> &'static str {
        match self {
            Self::Accepted { .. } => "accepted",
            Self::Timeout => "timeout",
            Self::WorkerFailed => "worker_failed",
            Self::Cancelled => "cancelled",
            Self::Stale => "stale",
            Self::Scope => "scope",
            Self::Generation => "generation",
        }
    }
}

pub(crate) struct CameraMetrics {
    scope: SourceScope,
    pub wall: AcceptedWall,
    pub report: AnalysisMetrics,
}

impl CameraMetrics {
    pub fn new(scope: SourceScope) -> Self {
        Self {
            scope,
            wall: AcceptedWall::default(),
            report: AnalysisMetrics {
                source_epoch: Counter(scope.epoch),
                dispatched_epochs: Counter(1),
                counts: InferenceCounts::default(),
                latency_bounds_us: LATENCY_BOUNDS_US,
                worker: WorkerDurations::default(),
                pose: TaskDurations::default(),
                fire: TaskDurations::default(),
            },
        }
    }

    pub fn dispatch(
        &mut self,
        scope: SourceScope,
        tasks: Tasks,
        skipped: [u64; 2],
        queue: Duration,
        at: Instant,
    ) {
        if scope != self.scope {
            self.scope = scope;
            self.report.source_epoch = Counter(scope.epoch);
            increment(&mut self.report.dispatched_epochs, 1);
        }
        self.wall.start(at);
        record_dispatch(&mut self.report.counts, tasks);
        record_misses(&mut self.report.counts, tasks, skipped);
        let queue_us = micros(queue);
        for_task(
            tasks,
            &mut self.report.pose,
            &mut self.report.fire,
            |_, step| {
                measure(&mut step.receive_to_dispatch_us, queue_us);
            },
        );
    }

    pub fn scope(&self) -> SourceScope {
        self.scope
    }

    pub fn worker_reply(
        &mut self,
        round_trip: Duration,
        timing: Option<ExecutionTiming>,
        tasks: Tasks,
    ) {
        let worker = &mut self.report.worker;
        measure(&mut worker.round_trip_us, micros(round_trip));
        if let Some(timing) = timing {
            measure(&mut worker.input_read_us, timing.input_read_us);
            measure(&mut worker.preprocess_us, timing.preprocess_us);
            measure(&mut worker.serialize_us, timing.serialize_us);
            if tasks.pose() {
                measure(&mut worker.pose_run_us, timing.pose_run_us);
                measure(&mut worker.pose_decode_us, timing.pose_decode_us);
            }
            if tasks.fire() {
                measure(&mut worker.fire_run_us, timing.fire_run_us);
                measure(&mut worker.fire_decode_us, timing.fire_decode_us);
            }
        }
    }

    pub fn accept(
        &mut self,
        tasks: Tasks,
        age: Duration,
        accepted_at: Instant,
        burst: bool,
    ) -> Outcome {
        let age_us = micros(age);
        for_task(
            tasks,
            &mut self.report.pose,
            &mut self.report.fire,
            |_, timing| {
                measure(&mut timing.receive_to_accept_us, age_us);
                timing.maximum_receive_to_accept_us =
                    Some(timing.maximum_receive_to_accept_us.unwrap_or(0).max(age_us));
            },
        );
        self.wall.record(tasks, accepted_at);
        Outcome::Accepted { burst }
    }
}

pub(crate) fn record_dispatch(counts: &mut InferenceCounts, tasks: Tasks) {
    for_task(tasks, &mut counts.pose, &mut counts.fire, |_, task| {
        increment(&mut task.dispatched, 1);
    });
}

pub(crate) fn record_misses(counts: &mut InferenceCounts, tasks: Tasks, missed: [u64; 2]) {
    for_task(tasks, &mut counts.pose, &mut counts.fire, |index, task| {
        increment(&mut task.missed_source_slots, missed[index]);
    });
}

pub(crate) fn record_outcome(counts: &mut InferenceCounts, tasks: Tasks, outcome: Outcome) {
    for_task(tasks, &mut counts.pose, &mut counts.fire, |index, task| {
        let field = match outcome {
            Outcome::Accepted { burst } => {
                if index == 1 && burst {
                    increment(&mut task.intentional_burst, 1);
                }
                &mut task.accepted
            }
            Outcome::Timeout => &mut task.timeout,
            Outcome::WorkerFailed => &mut task.worker_failed,
            Outcome::Cancelled => &mut task.cancelled,
            Outcome::Stale => &mut task.rejected_stale,
            Outcome::Scope => &mut task.rejected_scope,
            Outcome::Generation => &mut task.rejected_generation,
        };
        increment(field, 1);
    });
}

fn for_task<T>(tasks: Tasks, pose: &mut T, fire: &mut T, mut visit: impl FnMut(usize, &mut T)) {
    if tasks.pose() {
        visit(0, pose);
    }
    if tasks.fire() {
        visit(1, fire);
    }
}

fn increment(counter: &mut Counter, by: u64) {
    counter.0 = counter.0.saturating_add(by);
}

fn micros(duration: Duration) -> u32 {
    u32::try_from(duration.as_micros()).unwrap_or(u32::MAX)
}

fn measure(buckets: &mut LatencyDistribution, microseconds: u32) {
    let index = LATENCY_BOUNDS_US.partition_point(|&bound| bound < microseconds);
    increment(&mut buckets[index], 1);
}

#[derive(Default)]
struct WallSamples {
    instants: VecDeque<Instant>,
    latest_discarded: Option<Instant>,
}

impl WallSamples {
    fn push(&mut self, at: Instant) {
        while self
            .instants
            .front()
            .is_some_and(|&first| at - first >= WALL_WINDOW)
        {
            self.instants.pop_front();
        }
        if self.instants.len() == MAX_WALL_SAMPLES {
            self.latest_discarded = self.instants.pop_front();
        }
        self.instants.push_back(at);
    }

    fn count(&self, now: Instant) -> Option<u32> {
        if self
            .latest_discarded
            .is_some_and(|at| now - at < WALL_WINDOW)
        {
            return None;
        }
        let before = self.instants.partition_point(|&at| now - at >= WALL_WINDOW);
        Some((self.instants.len() - before) as u32)
    }
}

#[derive(Default)]
pub(crate) struct AcceptedWall {
    first: Option<Instant>,
    pose: WallSamples,
    fire: WallSamples,
}

impl AcceptedWall {
    fn start(&mut self, at: Instant) {
        self.first.get_or_insert(at);
    }

    fn record(&mut self, tasks: Tasks, at: Instant) {
        for_task(tasks, &mut self.pose, &mut self.fire, |_, samples| {
            samples.push(at)
        });
    }

    pub fn cadence(&self, now: Instant) -> Option<WallCadence> {
        if now - self.first? < WALL_WINDOW {
            return None;
        }
        Some(WallCadence {
            window_seconds: WALL_WINDOW.as_secs() as u32,
            pose_accepted: self.pose.count(now)?,
            fire_accepted: self.fire.count(now)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn completion_window_is_open_on_the_left_and_retains_outage_time() {
        let start = Instant::now();
        let mut wall = AcceptedWall::default();
        wall.start(start);
        wall.record(Tasks::Both, start);
        // A 15 fps source delivered at 7.5 fps. Accept Pose every third frame,
        // and Fire every eighth frame as the legacy source scheduler does.
        for frame in 1..=450 {
            let at = start + Duration::from_micros(frame * 2_000_000 / 15);
            match (frame % 3 == 0, frame % 8 == 0) {
                (true, true) => wall.record(Tasks::Both, at),
                (true, false) => wall.record(Tasks::Pose, at),
                (false, true) => wall.record(Tasks::Fire, at),
                (false, false) => {}
            }
        }
        assert!(
            wall.cadence(start + WALL_WINDOW - Duration::from_micros(1))
                .is_none()
        );
        let cadence = wall.cadence(start + WALL_WINDOW).unwrap();
        assert_eq!((cadence.pose_accepted, cadence.fire_accepted), (150, 56));
        let stalled = wall.cadence(start + WALL_WINDOW * 2).unwrap();
        assert_eq!((stalled.pose_accepted, stalled.fire_accepted), (0, 0));
    }

    #[test]
    fn epoch_transition_keeps_outcomes_and_does_not_bridge_source_clocks() {
        let start = Instant::now();
        let scope = SourceScope {
            camera_id: Uuid::new_v4(),
            epoch: 1,
            scene_revision: 1,
        };
        let mut metrics = CameraMetrics::new(scope);
        let task = Tasks::Fire;
        let queue = Duration::from_millis(75);
        let age = Duration::from_millis(500);
        for (scope, missed) in [
            (scope, 0),
            (scope, 2),
            (SourceScope { epoch: 2, ..scope }, 0),
        ] {
            metrics.dispatch(scope, task, [0, missed], queue, start);
            let outcome = metrics.accept(task, age, start, false);
            record_outcome(&mut metrics.report.counts, task, outcome);
        }
        metrics.worker_reply(
            Duration::from_millis(425),
            Some(ExecutionTiming {
                fire_run_us: 12_500,
                ..Default::default()
            }),
            task,
        );
        metrics.dispatch(metrics.scope, task, [0; 2], queue, start);
        record_misses(&mut metrics.report.counts, task, [0, 1]);
        record_outcome(&mut metrics.report.counts, task, Outcome::Timeout);
        let report = metrics.report;
        assert_eq!(report.dispatched_epochs, Counter(2));
        assert_eq!(report.source_epoch, Counter(2));
        assert_eq!(report.counts.fire.dispatched, Counter(4));
        assert_eq!(report.counts.fire.accepted, Counter(3));
        assert_eq!(report.counts.fire.timeout, Counter(1));
        assert_eq!(report.counts.fire.missed_source_slots, Counter(3));
        assert_eq!(report.counts.pose, Default::default());
        let threshold = LATENCY_BOUNDS_US
            .iter()
            .position(|&us| us == 500_000)
            .unwrap();
        assert_eq!(report.fire.receive_to_accept_us[threshold], Counter(3));
        assert_eq!(report.worker.round_trip_us[threshold], Counter(1));
        assert_eq!(report.fire.maximum_receive_to_accept_us, Some(500_000));
        assert_eq!(report.worker.pose_run_us, [Counter(0); 26]);
    }
}
