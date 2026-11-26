// Generated from sw-protocol schemas. Regenerate with npm run generate.

export type OnvifMedia = "media1" | "media2";

export interface OnvifStreamRequest {
  credential_ref?: string | null;
  device_endpoint: string;
  media: OnvifMedia;
  token: string;
}
