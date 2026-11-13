// Generated from sw-protocol schemas. Regenerate with npm run generate.

export type Category = "medical" | "fire" | "overview";
export type EvidenceLimitation =
  "partial_visibility" | "sparse_coverage" | "source_changed" | "pre_event_unavailable" | "non_live";
export type LocalFacts =
  | {
      apparent_growth: boolean;
      continuity_samples: number;
      controlled_region_overlap?: number | null;
      detector_confidence: number;
      escaped_controlled_region: boolean;
      flame_visible: boolean;
      smoke_growth: boolean;
      smoke_visible: boolean;
      type: "fire";
      visibility: EvidenceVisibility;
    }
  | {
      clear_eligible: boolean;
      episode_id: string;
      identity_uncertain: boolean;
      onset_observed: boolean;
      previous_episode_id?: string | null;
      recovery_seconds: number;
      track_id: Counter;
      type: "medical";
    };
export type EvidenceVisibility = "good" | "limited" | "poor";
/**
 * Canonical decimal u64 in 0..=18446744073709551615. Range is checked by the parser.
 */
export type Counter = string;
export type UtcTimestamp = string;
export type EvidenceImageAvailability = "available" | "unavailable";
export type EvidenceImageKind = "overview" | "crop";

export interface EvidencePacket {
  camera_id: string;
  category: Category;
  context?: EvidenceContext | null;
  evidence_version: Counter;
  expires_at: UtcTimestamp;
  images: EvidenceImageMetadata[];
  incident_id: string;
  packet_id: string;
  scene_revision: Counter;
  schema_version: string;
  source_epoch: Counter;
  [k: string]: unknown;
}
export interface EvidenceContext {
  coverage: EvidenceCoverage;
  limitations: EvidenceLimitation[];
  local_facts: LocalFacts;
  observed_interval: EvidenceInterval;
  policy_version: string;
  prompt_version: string;
}
export interface EvidenceCoverage {
  expected_samples: number;
  max_gap_ms: number;
  received_samples: number;
}
export interface EvidenceInterval {
  end_source_s: number;
  start_source_s: number;
}
export interface EvidenceImageMetadata {
  availability: EvidenceImageAvailability;
  image_id: string;
  kind: EvidenceImageKind;
  mime: string;
  ordinal: number;
  sha256?: string | null;
  source_s: number;
  [k: string]: unknown;
}
