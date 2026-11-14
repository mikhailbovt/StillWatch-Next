// Generated from sw-protocol schemas. Regenerate with npm run generate.

export interface Bootstrap {
  analysis_capacity_probe?: boolean;
  camera_main_stream?: boolean;
  camera_priority?: boolean;
  camera_probe_capacity?: boolean;
  camera_sampling?: boolean;
  instance_id: string;
  refresh_replay?: boolean;
  version: ApiVersion;
  [k: string]: unknown;
}
export interface ApiVersion {
  major: number;
  minor: number;
  [k: string]: unknown;
}
