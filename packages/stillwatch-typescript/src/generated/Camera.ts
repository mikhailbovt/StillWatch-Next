// Generated from sw-protocol schemas. Regenerate with npm run generate.

export type ControlledMode = "steady" | "intermittent";
export type ControlledSource = "burner" | "candle" | "fireplace" | "grill";
export type CameraSource =
  | {
      file: string;
      pace: boolean;
      repeat?: boolean;
      type: "replay";
    }
  | {
      credential_ref?: string | null;
      peer: string;
      secure: boolean;
      stream_path: string;
      transport: RtspTransport;
      type: "rtsp";
    }
  | {
      device_id: string;
      type: "usb";
    };
export type RtspTransport = "tcp" | "udp";
/**
 * Canonical decimal u64 in 0..=18446744073709551615. Range is checked by the parser.
 */
export type Counter = string;
export type AnalysisState = "disabled" | "starting" | "ready" | "degraded";
export type CaptureState =
  | "disabled"
  | "starting"
  | "replaying"
  | "streaming"
  | "clock_hold"
  | "completed"
  | "retrying"
  | "unavailable"
  | "capacity_exceeded";
export type SceneReview = "unconfigured" | "needs_review" | "confirmed";

export interface Camera {
  config: CameraConfig;
  dimensions?: Dimensions | null;
  id: string;
  live: boolean;
  revision: Counter;
  runtime: CameraRuntime;
  scene_review: SceneReview;
  scene_revision: Counter;
  source_epoch: Counter;
  [k: string]: unknown;
}
export interface CameraConfig {
  /**
   * Allows semantic evidence from this camera to be sent to Firebase AI Logic.
   */
  cloud_consent?: boolean;
  /**
   * @maxItems 16
   */
  controlled_fire_regions?:
    | []
    | [ControlledFireRegion]
    | [ControlledFireRegion, ControlledFireRegion]
    | [ControlledFireRegion, ControlledFireRegion, ControlledFireRegion]
    | [ControlledFireRegion, ControlledFireRegion, ControlledFireRegion, ControlledFireRegion]
    | [ControlledFireRegion, ControlledFireRegion, ControlledFireRegion, ControlledFireRegion, ControlledFireRegion]
    | [
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion
      ]
    | [
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion
      ]
    | [
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion
      ]
    | [
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion
      ]
    | [
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion
      ]
    | [
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion
      ]
    | [
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion
      ]
    | [
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion
      ]
    | [
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion
      ]
    | [
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion
      ]
    | [
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion,
        ControlledFireRegion
      ];
  enabled: boolean;
  name: string;
  privacy_masks?: MaskRect[];
  sampling?: CameraSampling;
  source: CameraSource;
}
export interface ControlledFireRegion {
  mode: ControlledMode;
  /**
   * @minItems 3
   * @maxItems 16
   */
  polygon:
    | [RegionPoint, RegionPoint, RegionPoint]
    | [RegionPoint, RegionPoint, RegionPoint, RegionPoint]
    | [RegionPoint, RegionPoint, RegionPoint, RegionPoint, RegionPoint]
    | [RegionPoint, RegionPoint, RegionPoint, RegionPoint, RegionPoint, RegionPoint]
    | [RegionPoint, RegionPoint, RegionPoint, RegionPoint, RegionPoint, RegionPoint, RegionPoint]
    | [RegionPoint, RegionPoint, RegionPoint, RegionPoint, RegionPoint, RegionPoint, RegionPoint, RegionPoint]
    | [
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint
      ]
    | [
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint
      ]
    | [
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint
      ]
    | [
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint
      ]
    | [
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint
      ]
    | [
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint
      ]
    | [
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint
      ]
    | [
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint,
        RegionPoint
      ];
  source: ControlledSource;
}
export interface RegionPoint {
  x: number;
  y: number;
}
export interface MaskRect {
  bottom: number;
  left: number;
  right: number;
  top: number;
}
/**
 * Omit to use 5 pose, 2 fire and 5 evidence frames per second.
 */
export interface CameraSampling {
  evidence_fps: number;
  fire_fps: number;
  pose_fps: number;
}
export interface Dimensions {
  height: number;
  width: number;
  [k: string]: unknown;
}
export interface CameraRuntime {
  analysis?: AnalysisStatus | null;
  codec?: string | null;
  dropped: Counter;
  frame?: Counter | null;
  last_frame_age_ms?: Counter | null;
  repeated_frame?: boolean;
  state: CaptureState;
  [k: string]: unknown;
}
export interface AnalysisStatus {
  cadence?: AnalysisCadence | null;
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
export interface AnalysisCadence {
  below_target: boolean;
  fire_frames_per_minute: number;
  fire_target_per_minute: number;
  /**
   * Accepted model results in the last ten seconds, normalized by source time.
   */
  pose_frames_per_minute: number;
  pose_target_per_minute: number;
  [k: string]: unknown;
}
