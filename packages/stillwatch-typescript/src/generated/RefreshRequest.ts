// Generated from sw-protocol schemas. Regenerate with npm run generate.

export interface RefreshRequest {
  device_id: string;
  nonce: string;
  refresh_token: string;
  signature: string;
}
