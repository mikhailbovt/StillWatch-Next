// Generated from sw-protocol schemas. Regenerate with npm run generate.

export interface SourceProbe {
  capture_capacity?: CaptureCapacity | null;
  codec?: string | null;
  dimensions: Dimensions;
  live: boolean;
  native_pts: boolean;
  [k: string]: unknown;
}
export interface CaptureCapacity {
  available_camera_slots: number;
  available_raw_bytes: number;
  fits: boolean;
  required_raw_bytes: number;
  [k: string]: unknown;
}
export interface Dimensions {
  height: number;
  width: number;
  [k: string]: unknown;
}
