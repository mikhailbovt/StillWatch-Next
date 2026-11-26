// Generated from sw-protocol schemas. Regenerate with npm run generate.

export type UtcTimestamp = string;
/**
 * Canonical decimal u64 in 0..=18446744073709551615. Range is checked by the parser.
 */
export type Counter = string;

export interface HealthEventPage {
  events: HealthEvent[];
  next?: Counter | null;
  [k: string]: unknown;
}
export interface HealthEvent {
  camera_id?: string | null;
  /**
   * Stable event code. Clients display unfamiliar codes as generic technical events.
   */
  code: string;
  ended_at?: UtcTimestamp | null;
  opened_at: UtcTimestamp;
  sequence: Counter;
  [k: string]: unknown;
}
