// Generated from sw-protocol schemas. Regenerate with npm run generate.

export type UtcTimestamp = string;
export type DeviceRole = "owner" | "operator" | "viewer";
export type DeviceScope = "cameras_write" | "events_ack" | "evidence_read" | "streams_read" | "provider_evaluate";
export type ArrayOf_Device = Device[];

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
