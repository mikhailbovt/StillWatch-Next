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
export interface Dimensions {
  height: number;
  width: number;
  [k: string]: unknown;
}
export interface CameraRuntime {
  dropped: Counter;
  frame?: Counter | null;
  last_frame_age_ms?: Counter | null;
  state: CaptureState;
  [k: string]: unknown;
}
