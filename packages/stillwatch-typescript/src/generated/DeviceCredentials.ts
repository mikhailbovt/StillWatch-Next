// Generated from sw-protocol schemas. Regenerate with npm run generate.

export type UtcTimestamp = string;
export type DeviceRole = "owner" | "operator" | "viewer";
export type DeviceScope = "cameras_write" | "events_ack" | "evidence_read" | "streams_read" | "provider_evaluate";

export interface DeviceCredentials {
  access_expires_at: UtcTimestamp;
  access_token: string;
  device: Device;
  refresh_expires_at: UtcTimestamp;
  refresh_token: string;
  [k: string]: unknown;
}
export interface Device {
  camera_ids: string[];
  id: string;
  label: string;
  paired_at: UtcTimestamp;
  revoked_at?: UtcTimestamp | null;
  role: DeviceRole;
  scopes: DeviceScope[];
  [k: string]: unknown;
}
