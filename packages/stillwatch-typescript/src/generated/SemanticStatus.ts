// Generated from sw-protocol schemas. Regenerate with npm run generate.

export type AppCheckState = "ready" | "renewal_due" | "development_debug" | "unavailable" | "unknown";
export type SemanticState = "disabled" | "connecting" | "available" | "degraded";

export interface SemanticStatus {
  app_check_state?: AppCheckState | null;
  cloud_model?: string | null;
  cloud_route_ready?: boolean | null;
  daily_request_limit: number;
  last_inference_source?: string | null;
  local_model_state?: string | null;
  route_ready?: boolean | null;
  sdk_version?: string | null;
  state: SemanticState;
  used_today: number;
  [k: string]: unknown;
}
