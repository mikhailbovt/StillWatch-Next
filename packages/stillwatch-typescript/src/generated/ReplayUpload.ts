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
  dimensions: Dimensions;
  live: boolean;
  native_pts: boolean;
  [k: string]: unknown;
}
export interface Dimensions {
  height: number;
  width: number;
  [k: string]: unknown;
}
