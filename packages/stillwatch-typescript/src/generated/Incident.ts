// Generated from sw-protocol schemas. Regenerate with npm run generate.

export type UtcTimestamp = string;
export type Assessment = "unknown" | "no_visible_concern" | "concern" | "immediate_visible_concern";
export type Attention = "info" | "review" | "urgent";
/**
 * Canonical decimal u64 in 0..=18446744073709551615. Range is checked by the parser.
 */
export type Counter = string;
export type Category = "medical" | "fire" | "overview";
export type EvidenceState = "pending" | "persisted" | "partial" | "unavailable";
export type Lifecycle = "open" | "resolved";

export interface Incident {
  acknowledgement?: Acknowledgement | null;
  analysis_origin?: AnalysisOrigin | null;
  assessment: Assessment;
  attention: Attention;
  attention_cycle: Counter;
  camera_id: string;
  category: Category;
  created_at: UtcTimestamp;
  detection_reason: string;
  evidence_packet_id?: string | null;
  evidence_state: EvidenceState;
  evidence_version: Counter;
  id: string;
  lifecycle: Lifecycle;
  live: boolean;
  needs_reassessment?: boolean;
  opening_delayed?: boolean;
  opening_observed_host_utc?: UtcTimestamp | null;
  possible_continuation_of?: UncertainIncidentLink | null;
  resolution?: Resolution | null;
  revision: Counter;
  scene_revision: Counter;
  source_epoch: Counter;
  updated_at: UtcTimestamp;
  [k: string]: unknown;
}
export interface Acknowledgement {
  acknowledged_at: UtcTimestamp;
  actor_id: string;
  [k: string]: unknown;
}
export interface AnalysisOrigin {
  model_manifest_sha256: string;
  policy_version: string;
  [k: string]: unknown;
}
export interface UncertainIncidentLink {
  incident_id: string;
  source_epoch: Counter;
  [k: string]: unknown;
}
export interface Resolution {
  actor_id?: string | null;
  reason: string;
  resolved_at: UtcTimestamp;
  [k: string]: unknown;
}
