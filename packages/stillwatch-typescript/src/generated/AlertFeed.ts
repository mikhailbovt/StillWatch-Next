// Generated from sw-protocol schemas. Regenerate with npm run generate.

export type Attention = "info" | "review" | "urgent";
/**
 * Canonical decimal u64 in 0..=18446744073709551615. Range is checked by the parser.
 */
export type Counter = string;
export type UtcTimestamp = string;

export interface AlertFeed {
  alerts: AlertDelivery[];
  [k: string]: unknown;
}
export interface AlertDelivery {
  attention: Attention;
  attention_cycle: Counter;
  camera_id: string;
  client_seen_at?: UtcTimestamp | null;
  core_id: string;
  expires_at: UtcTimestamp;
  id: Counter;
  incident_id: string;
  revision: Counter;
  [k: string]: unknown;
}
