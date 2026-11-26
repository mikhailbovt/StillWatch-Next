// Generated from sw-protocol schemas. Regenerate with npm run generate.

export type OnvifMedia = "media1" | "media2";
export type ArrayOf_OnvifProfile = OnvifProfile[];

export interface OnvifProfile {
  media: OnvifMedia;
  name: string;
  token: string;
}
