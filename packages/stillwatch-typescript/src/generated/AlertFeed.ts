// Generated from sw-protocol schemas. Regenerate with npm run generate.

export type Attention = "info" | "review" | "urgent";
/**
 * Canonical decimal u64 in 0..=18446744073709551615. Range is checked by the parser.
 */
export type Counter = string;
export type UtcTimestamp = string;

export interface AlertFeed {
  alerts: AlertDelivery[];
  /**
   * Changes when Core invalidates all delivery state during restore.
   */
  generation?: string | null;
  /**
   * Highest notification ID included in this scan.
   */
  head?: Counter | null;
  /**
   * Device-scoped continuation for the current feed scan.
   */
  next?: string | null;
  /**
   * Server time at which this page was evaluated.
   */
  observed_at?: UtcTimestamp | null;
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
