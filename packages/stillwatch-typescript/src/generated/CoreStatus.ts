// Generated from sw-protocol schemas. Regenerate with npm run generate.

/**
 * Canonical decimal u64 in 0..=18446744073709551615. Range is checked by the parser.
 */
export type Counter = string;
export type AnalysisState = "disabled" | "starting" | "ready" | "degraded";
export type CaptureState =
  | "disabled"
  | "starting"
  | "replaying"
  | "streaming"
  | "clock_hold"
  | "completed"
  | "retrying"
  | "unavailable"
  | "capacity_exceeded";
export type InferenceBackend = "cpu" | "cuda" | "unrecognized";
export type InferenceFailure =
  | "model_pack_unavailable"
  | "model_initialization_failed"
  | "runtime_unavailable"
  | "worker_unavailable"
  | "worker_timeout"
  | "worker_failed"
  | "evidence_backpressure"
  | "unrecognized";

export interface CoreStatus {
  /**
   * Bounded local snapshot sized by the camera registry.
   */
  camera_coverage?: CameraCoverage[] | null;
  inference: InferenceStatus;
  media?: MediaStatus | null;
  version: string;
  writer?: WriterStatus | null;
  [k: string]: unknown;
}
export interface CameraCoverage {
  analysis: AnalysisCoverage;
  camera_id: string;
  capture_state: CaptureState;
  current_source_epoch?: Counter | null;
  last_frame_age_ms?: Counter | null;
  revision: Counter;
  source_lagging?: boolean | null;
  [k: string]: unknown;
}
export interface AnalysisCoverage {
  fire_frames: Counter;
  last_analysis_age_ms?: Counter | null;
  pose_frames: Counter;
  state: AnalysisState;
  wall_cadence?: WallCadence | null;
  [k: string]: unknown;
}
export interface WallCadence {
  fire_accepted: number;
  pose_accepted: number;
  /**
   * Accepted results in the trailing monotonic clock interval.
   */
  window_seconds: number;
  [k: string]: unknown;
}
export interface InferenceStatus {
  backend?: InferenceBackend | null;
  configured_workers: number;
  failure?: InferenceFailure | null;
  ready_workers: number;
  state: AnalysisState;
  /**
   * Task totals since Core started. Visible to the owner.
   */
  totals?: InferenceCounts | null;
  [k: string]: unknown;
}
export interface InferenceCounts {
  fire: TaskCounts;
  pose: TaskCounts;
  [k: string]: unknown;
}
export interface TaskCounts {
  accepted: Counter;
  cancelled: Counter;
  dispatched: Counter;
  /**
   * Canonical decimal u64 in 0..=18446744073709551615. Range is checked by the parser.
   */
  intentional_burst: string;
  /**
   * Canonical decimal u64 in 0..=18446744073709551615. Range is checked by the parser.
   */
  missed_source_slots: string;
  rejected_generation: Counter;
  rejected_scope: Counter;
  rejected_stale: Counter;
  timeout: Counter;
  worker_failed: Counter;
  [k: string]: unknown;
}
export interface MediaStatus {
  redacted_transcode: boolean;
  router_ready: boolean;
  video_codec: string;
  [k: string]: unknown;
}
export interface WriterStatus {
  cancelled_queued: Counter;
  completed: Counter;
  maximum_operation_ms: Counter;
  maximum_queue_wait_ms: Counter;
  oldest_queued_ms?: Counter | null;
  queue_capacity: number;
  queue_high_water: number;
  queued: number;
  rejected_queue_full: Counter;
  running_ms?: Counter | null;
  [k: string]: unknown;
}
