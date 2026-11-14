// Generated from sw-protocol schemas. Regenerate with npm run generate.

export type CameraPriority = "normal" | "high";
export type CameraSource =
  | {
      file: string;
      pace: boolean;
      repeat?: boolean;
      type: "replay";
    }
  | {
      credential_ref?: string | null;
      /**
       * Optional detail stream on the same camera. Both paths share the camera's masks.
       */
      main_stream_path?: string | null;
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
  priority?: CameraPriority;
  privacy_masks?: MaskRect[];
  sampling?: CameraSampling;
  source: CameraSource;
}
export interface ControlledFireRegion {
  /**
   * Operator annotation for its expected duty cycle.
   */
  mode: "steady" | "intermittent";
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
  /**
   * Operator annotation for the expected source within the polygon.
   */
  source: "burner" | "candle" | "fireplace" | "grill";
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
  /**
   * The confirmed polygon is suspended because the current view could not be aligned.
   */
  controlled_scene_suspended?: boolean;
  dropped: Counter;
  /**
   * Native presentation timestamp in microseconds of the first usable frame in the epoch.
   */
  first_source_us?: string | null;
  frame?: Counter | null;
  last_frame_age_ms?: Counter | null;
  /**
   * Signed change in source lag since the first usable native PTS of this live or paced epoch.
   */
  relative_source_lag_ms?: number | null;
  repeated_frame?: boolean;
  /**
   * Sustained relative lag with hysteresis, omitted until measured.
   */
  source_lagging?: boolean | null;
  state: CaptureState;
  [k: string]: unknown;
}
export interface AnalysisStatus {
  cadence?: AnalysisCadence | null;
  /**
   * Accepted fire inferences dispatched ahead of the configured base interval.
   */
  extra_fire_frames?: Counter | null;
  fire_frames: Counter;
  fire_regions?: number | null;
  last_analysis_age_ms?: Counter | null;
  last_fire_source_us?: string | null;
  /**
   * Native presentation timestamp in microseconds, as a signed decimal string.
   */
  last_pose_source_us?: string | null;
  /**
   * Time between receiving the last accepted frame and dispatching it to a worker.
   */
  last_queue_age_ms?: number | null;
  last_worker_ms?: number | null;
  people?: number | null;
  pose_frames: Counter;
  smoke_regions?: number | null;
  source_epoch?: Counter | null;
  state: AnalysisState;
  wall_cadence?: WallCadence | null;
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
export interface WallCadence {
  fire_accepted: number;
  pose_accepted: number;
  /**
   * Accepted results in the trailing monotonic clock interval.
   */
  window_seconds: number;
  [k: string]: unknown;
}
