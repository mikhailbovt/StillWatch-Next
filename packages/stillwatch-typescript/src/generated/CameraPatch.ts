// Generated from sw-protocol schemas. Regenerate with npm run generate.

export type ControlledMode = "steady" | "intermittent";
export type ControlledSource = "burner" | "candle" | "fireplace" | "grill";
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

export interface CameraPatch {
  cloud_consent?: boolean | null;
  controlled_fire_regions?: ControlledFireRegion[] | null;
  enabled?: boolean | null;
  name?: string | null;
  priority?: CameraPriority | null;
  privacy_masks?: MaskRect[] | null;
  sampling?: CameraSampling | null;
  source?: CameraSource | null;
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
export interface CameraSampling {
  evidence_fps: number;
  fire_fps: number;
  pose_fps: number;
}
