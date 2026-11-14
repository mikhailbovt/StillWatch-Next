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
