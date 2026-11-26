// Generated from sw-protocol schemas. Regenerate with npm run generate.

export type UtcTimestamp = string;
export type StreamProfile = "preview" | "detail";
/**
 * Canonical decimal u64 in 0..=18446744073709551615. Range is checked by the parser.
 */
export type Counter = string;

export interface StreamGrant {
  camera_id: string;
  codec: string;
  expires_at: UtcTimestamp;
  ice_servers?: IceServer[];
  id: string;
  profile: StreamProfile;
  redacted: boolean;
  scene_revision: Counter;
  source_epoch: Counter;
  whep_path: string;
  [k: string]: unknown;
}
export interface IceServer {
  credential: string;
  url: string;
  username: string;
  [k: string]: unknown;
}
