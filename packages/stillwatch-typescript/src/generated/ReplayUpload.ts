// Generated from sw-protocol schemas. Regenerate with npm run generate.

/**
 * Canonical decimal u64 in 0..=18446744073709551615. Range is checked by the parser.
 */
export type Counter = string;

export interface ReplayUpload {
  bytes: Counter;
  file: string;
  source_probe: SourceProbe;
  [k: string]: unknown;
}
export interface SourceProbe {
  capture_capacity?: CaptureCapacity | null;
  codec?: string | null;
  dimensions: Dimensions;
  live: boolean;
  main_stream?: MainStreamProbe | null;
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
export interface MainStreamProbe {
  codec?: string | null;
  dimensions: Dimensions;
  native_pts: boolean;
  [k: string]: unknown;
}
