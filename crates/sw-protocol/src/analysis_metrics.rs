use serde::{Deserialize, Serialize};

use crate::Counter;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct InferenceCounts {
    pub pose: TaskCounts,
    pub fire: TaskCounts,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TaskCounts {
    pub dispatched: Counter,
    pub accepted: Counter,
    pub timeout: Counter,
    pub worker_failed: Counter,
    pub cancelled: Counter,
    pub rejected_stale: Counter,
    pub rejected_scope: Counter,
    pub rejected_generation: Counter,
    /// Skipped base slots before a dispatch and rejected base claims within one source clock.
    pub missed_source_slots: Counter,
    /// Accepted results from intentionally early Fire dispatches.
    pub intentional_burst: Counter,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AnalysisMetrics {
    /// Source epoch of the most recent dispatch. Totals include previous epochs.
    pub source_epoch: Counter,
    /// Number of source epochs with a dispatch while this camera has been tracked.
    pub dispatched_epochs: Counter,
    pub counts: InferenceCounts,
    /// Inclusive upper bounds in microseconds. Each distribution has an extra overflow bucket.
    pub latency_bounds_us: [u32; 25],
    pub worker: WorkerDurations,
    pub pose: TaskDurations,
    pub fire: TaskDurations,
}

/// Counts per disjoint bucket, in ascending order of the inclusive upper bound.
pub type LatencyDistribution = [Counter; 26];

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WorkerDurations {
    pub round_trip_us: LatencyDistribution,
    pub input_read_us: LatencyDistribution,
    pub preprocess_us: LatencyDistribution,
    pub pose_run_us: LatencyDistribution,
    pub pose_decode_us: LatencyDistribution,
    pub fire_run_us: LatencyDistribution,
    pub fire_decode_us: LatencyDistribution,
    pub serialize_us: LatencyDistribution,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TaskDurations {
    pub receive_to_dispatch_us: LatencyDistribution,
    pub receive_to_accept_us: LatencyDistribution,
    pub maximum_receive_to_accept_us: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WallCadence {
    /// Accepted results in the trailing monotonic clock interval.
    pub window_seconds: u32,
    pub pose_accepted: u32,
    pub fire_accepted: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CameraCoverage {
    pub camera_id: uuid::Uuid,
    pub revision: Counter,
    pub capture_state: crate::CaptureState,
    pub current_source_epoch: Option<Counter>,
    pub last_frame_age_ms: Option<Counter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_lagging: Option<bool>,
    pub analysis: AnalysisCoverage,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AnalysisCoverage {
    pub state: crate::AnalysisState,
    pub pose_frames: Counter,
    pub fire_frames: Counter,
    pub last_analysis_age_ms: Option<Counter>,
    pub wall_cadence: Option<WallCadence>,
}
