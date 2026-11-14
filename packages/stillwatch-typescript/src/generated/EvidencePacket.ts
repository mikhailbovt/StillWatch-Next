// Generated from sw-protocol schemas. Regenerate with npm run generate.

export type Category = "medical" | "fire" | "overview";
export type EvidenceLimitation =
  | "partial_visibility"
  | "sparse_coverage"
  | "source_changed"
  | "pre_event_unavailable"
  | "non_live"
  | "observation_gap";
export type EpisodeAssociation = "new" | "continuous" | "reobserved" | "split" | "uncertain";
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
export type EvidenceRecovery = "upright_activity" | "prior_supported_posture" | "controlled_activity";
export type UtcTimestamp = string;
export type EvidenceImageAvailability = "available" | "unavailable";
export type EvidenceImageKind = "overview" | "crop";

export interface EvidencePacket {
  camera_id: string;
  category: Category;
  context?: EvidenceContext | null;
  evidence_version: Counter;
  expires_at: UtcTimestamp;
  /**
   * Freshness at persistence. Live semantic dispatch rechecks the current deadline.
   */
  fresh_when_persisted?: boolean | null;
  images: EvidenceImageMetadata[];
  incident_id: string;
  observed_host_utc?: UtcTimestamp | null;
  packet_id: string;
  persisted_at?: UtcTimestamp | null;
  scene_revision: Counter;
  schema_version: string;
  source_epoch: Counter;
  [k: string]: unknown;
}
export interface EvidenceContext {
  coverage: EvidenceCoverage;
  limitations: EvidenceLimitation[];
  lineage?: EvidenceLineage | null;
  local_facts: LocalFacts;
  observed_interval: EvidenceInterval;
  policy_version: string;
  prompt_version: string;
  recovery?: EvidenceRecovery | null;
  sampling?: EvidenceSampling | null;
}
export interface EvidenceCoverage {
  expected_samples: number;
  max_gap_ms: number;
  received_samples: number;
}
export interface EvidenceLineage {
  association: EpisodeAssociation;
  episode_id: string;
  fire_track_id?: string | null;
}
export interface EvidenceInterval {
  end_source_s: number;
  start_source_s: number;
}
export interface EvidenceSampling {
  attached_crops: number;
  attached_overviews: number;
  configured_fps: number;
  distinct_frames: number;
  motion_coverage: boolean;
  window_ms: number;
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
