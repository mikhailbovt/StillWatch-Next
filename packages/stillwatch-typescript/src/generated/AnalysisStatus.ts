// Generated from sw-protocol schemas. Regenerate with npm run generate.

/**
 * Canonical decimal u64 in 0..=18446744073709551615. Range is checked by the parser.
 */
export type Counter = string;
export type AnalysisState = "disabled" | "starting" | "ready" | "degraded";

export interface AnalysisStatus {
  fire_frames: Counter;
  fire_regions?: number | null;
  last_analysis_age_ms?: Counter | null;
  last_worker_ms?: number | null;
  people?: number | null;
  pose_frames: Counter;
  smoke_regions?: number | null;
  source_epoch?: Counter | null;
  state: AnalysisState;
  [k: string]: unknown;
}
