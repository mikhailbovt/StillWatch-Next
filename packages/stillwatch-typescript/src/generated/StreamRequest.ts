// Generated from sw-protocol schemas. Regenerate with npm run generate.

export type StreamProfile = "preview" | "detail";

export interface StreamRequest {
  camera_id: string;
  profile: StreamProfile;
}
