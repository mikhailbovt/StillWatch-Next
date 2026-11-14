// Generated from sw-protocol schemas. Regenerate with npm run generate.

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

export interface SourceProbeRequest {
  /**
   * Replace this camera's reservation in the capacity estimate.
   */
  camera_id?: string | null;
  privacy_masks?: MaskRect[];
  source: CameraSource;
}
export interface MaskRect {
  bottom: number;
  left: number;
  right: number;
  top: number;
}
