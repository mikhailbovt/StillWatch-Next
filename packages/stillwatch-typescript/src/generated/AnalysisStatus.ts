// Generated from sw-protocol schemas. Regenerate with npm run generate.

/**
 * Canonical decimal u64 in 0..=18446744073709551615. Range is checked by the parser.
 */
export type Counter = string;
export type AnalysisState = "disabled" | "starting" | "ready" | "degraded";

export interface AnalysisStatus {
  cadence?: AnalysisCadence | null;
  /**
   * Accepted fire inferences dispatched ahead of the configured base interval.
   */
  extra_fire_frames?: Counter | null;
  fire_frames: Counter;
  fire_regions?: number | null;
  last_analysis_age_ms?: Counter | null;
  last_fire_source_us?: string | null;
  /**
   * Native presentation timestamp in microseconds, as a signed decimal string.
   */
  last_pose_source_us?: string | null;
  /**
   * Time between receiving the last accepted frame and dispatching it to a worker.
   */
  last_queue_age_ms?: number | null;
  last_worker_ms?: number | null;
  people?: number | null;
  pose_frames: Counter;
  smoke_regions?: number | null;
  source_epoch?: Counter | null;
  state: AnalysisState;
  [k: string]: unknown;
}
export interface AnalysisCadence {
  below_target: boolean;
  fire_frames_per_minute: number;
  fire_target_per_minute: number;
  /**
   * Accepted model results in the last ten seconds, normalized by source time.
   */
  pose_frames_per_minute: number;
  pose_target_per_minute: number;
  [k: string]: unknown;
}
