// Generated from sw-protocol schemas. Regenerate with npm run generate.

export type SourceKind = "paced_replay" | "unpaced_replay" | "rtsp_tcp" | "rtsp_udp" | "rtsps" | "usb";
export type CapacityState = "verified" | "unknown" | "insufficient";
export type SourceRateQuality = "unknown" | "observed" | "variable";

export interface SourceProbe {
  analysis_capacity?: AnalysisCapacity | null;
  capture_capacity?: CaptureCapacity | null;
  codec?: string | null;
  dimensions: Dimensions;
  live: boolean;
  main_stream?: MainStreamProbe | null;
  native_pts: boolean;
  source_rate?: SourceRate | null;
  [k: string]: unknown;
}
export interface AnalysisCapacity {
  admissible_workload?: AnalysisWorkload | null;
  available_camera_slots?: number | null;
  limiting_resource?: string | null;
  profile_id?: string | null;
  profile_sha256?: string | null;
  reason: string;
  state: CapacityState;
  [k: string]: unknown;
}
export interface AnalysisWorkload {
  cameras: number;
  detail_source?: SourceWorkload | null;
  media: MediaWorkloadLimits;
  min_fire_per_wall_minute: number;
  min_pose_per_wall_minute: number;
  sampling: CameraSampling;
  source: SourceWorkload;
}
/**
 * Named profiles admit only sources whose decoder workload matches this class.
 */
export interface SourceWorkload {
  codec: string;
  dimensions: Dimensions;
  kind: SourceKind;
  max_millifps: number;
  min_millifps: number;
}
export interface Dimensions {
  height: number;
  width: number;
  [k: string]: unknown;
}
/**
 * Simultaneous allowances on the host, including both encoder and decoder work.
 */
export interface MediaWorkloadLimits {
  detail_decoder_units: number;
  detail_decoders: number;
  detail_publishers: number;
  detail_units: number;
  preview_publishers: number;
  preview_units: number;
  publisher_units: number;
  snapshot_encoders: number;
  snapshot_requests_per_minute: number;
  viewer_sessions: number;
}
export interface CameraSampling {
  evidence_fps: number;
  fire_fps: number;
  pose_fps: number;
}
export interface CaptureCapacity {
  available_camera_slots: number;
  available_raw_bytes: number;
  fits: boolean;
  required_raw_bytes: number;
  [k: string]: unknown;
}
export interface MainStreamProbe {
  codec?: string | null;
  dimensions: Dimensions;
  native_pts: boolean;
  source_rate?: SourceRate | null;
  [k: string]: unknown;
}
export interface SourceRate {
  advertised_millifps?: number | null;
  observed_millifps?: number | null;
  quality: SourceRateQuality;
  [k: string]: unknown;
}
