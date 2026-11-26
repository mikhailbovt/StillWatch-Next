// Generated from sw-protocol schemas. Regenerate with npm run generate.

export type UtcTimestamp = string;

export interface PairingOffer {
  api: ApiVersion;
  challenge: string;
  core_instance_id: string;
  endpoint: string;
  expires_at: UtcTimestamp;
  secret: string;
  /**
   * Base64url DER for the explicitly pinned, self-signed endpoint certificate.
   */
  tls_certificate_der: string;
  tls_certificate_sha256: string;
}
export interface ApiVersion {
  major: number;
  minor: number;
  [k: string]: unknown;
}
