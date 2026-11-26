// Generated from sw-protocol schemas. Regenerate with npm run generate.

export type DeviceRole = "owner" | "operator" | "viewer";
export type DeviceScope = "cameras_write" | "events_ack" | "evidence_read" | "streams_read" | "provider_evaluate";

export interface PairingRequest {
  /**
   * An empty list grants access to all cameras for an owner and none for other roles.
   */
  camera_ids: string[];
  role: DeviceRole;
  scopes: DeviceScope[];
}
