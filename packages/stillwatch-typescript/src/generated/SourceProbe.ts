// Generated from sw-protocol schemas. Regenerate with npm run generate.

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
