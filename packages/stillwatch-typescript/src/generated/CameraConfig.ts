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
