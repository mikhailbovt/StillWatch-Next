// Generated from sw-protocol schemas. Regenerate with npm run generate.

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
export type AnalysisState = "disabled" | "starting" | "ready" | "degraded";
/**
 * Canonical decimal u64 in 0..=18446744073709551615. Range is checked by the parser.
 */
export type Counter = string;

export interface CoreStatus {
  inference: InferenceStatus;
  media?: MediaStatus | null;
  version: string;
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
