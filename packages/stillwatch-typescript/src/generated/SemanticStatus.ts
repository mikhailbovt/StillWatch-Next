// Generated from sw-protocol schemas. Regenerate with npm run generate.

export type SemanticState = "disabled" | "connecting" | "available" | "degraded";

export interface SemanticStatus {
  cloud_model?: string | null;
  daily_request_limit: number;
  last_inference_source?: string | null;
  local_model_state?: string | null;
  sdk_version?: string | null;
  state: SemanticState;
  used_today: number;
  [k: string]: unknown;
}
