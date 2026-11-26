// Generated from sw-protocol schemas. Regenerate with npm run generate.

export type HealthState = "live" | "ready" | "degraded";

export interface Health {
  api: ApiVersion;
  state: HealthState;
  [k: string]: unknown;
}
export interface ApiVersion {
  major: number;
  minor: number;
  [k: string]: unknown;
}
