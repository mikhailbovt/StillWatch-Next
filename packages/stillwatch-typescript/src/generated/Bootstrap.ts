// Generated from sw-protocol schemas. Regenerate with npm run generate.

export interface Bootstrap {
  camera_probe_capacity?: boolean;
  camera_sampling?: boolean;
  instance_id: string;
  version: ApiVersion;
  [k: string]: unknown;
}
export interface ApiVersion {
  major: number;
  minor: number;
  [k: string]: unknown;
}
