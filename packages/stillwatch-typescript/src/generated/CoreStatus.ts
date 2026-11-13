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
  [k: string]: unknown;
}
export interface MediaStatus {
  redacted_transcode: boolean;
  router_ready: boolean;
  video_codec: string;
  [k: string]: unknown;
}
