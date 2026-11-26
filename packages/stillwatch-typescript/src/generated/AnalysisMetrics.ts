// Generated from sw-protocol schemas. Regenerate with npm run generate.

/**
 * Canonical decimal u64 in 0..=18446744073709551615. Range is checked by the parser.
 */
export type Counter = string;

export interface AnalysisMetrics {
  counts: InferenceCounts;
  /**
   * Canonical decimal u64 in 0..=18446744073709551615. Range is checked by the parser.
   */
  dispatched_epochs: string;
  fire: TaskDurations;
  /**
   * Inclusive upper bounds in microseconds. Each distribution has an extra overflow bucket.
   *
   * @minItems 25
   * @maxItems 25
   */
  latency_bounds_us: [
    number,
    number,
    number,
    number,
    number,
    number,
    number,
    number,
    number,
    number,
    number,
    number,
    number,
    number,
    number,
    number,
    number,
    number,
    number,
    number,
    number,
    number,
    number,
    number,
    number
  ];
  pose: TaskDurations;
  /**
   * Canonical decimal u64 in 0..=18446744073709551615. Range is checked by the parser.
   */
  source_epoch: string;
  worker: WorkerDurations;
  [k: string]: unknown;
}
export interface InferenceCounts {
  fire: TaskCounts;
  pose: TaskCounts;
  [k: string]: unknown;
}
export interface TaskCounts {
  accepted: Counter;
  cancelled: Counter;
  dispatched: Counter;
  /**
   * Canonical decimal u64 in 0..=18446744073709551615. Range is checked by the parser.
   */
  intentional_burst: string;
  /**
   * Canonical decimal u64 in 0..=18446744073709551615. Range is checked by the parser.
   */
  missed_source_slots: string;
  rejected_generation: Counter;
  rejected_scope: Counter;
  rejected_stale: Counter;
  timeout: Counter;
  worker_failed: Counter;
  [k: string]: unknown;
}
export interface TaskDurations {
  maximum_receive_to_accept_us?: number | null;
  /**
   * @minItems 26
   * @maxItems 26
   */
  receive_to_accept_us: [
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter
  ];
  /**
   * @minItems 26
   * @maxItems 26
   */
  receive_to_dispatch_us: [
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter
  ];
  [k: string]: unknown;
}
export interface WorkerDurations {
  /**
   * @minItems 26
   * @maxItems 26
   */
  fire_decode_us: [
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter
  ];
  /**
   * @minItems 26
   * @maxItems 26
   */
  fire_run_us: [
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter
  ];
  /**
   * @minItems 26
   * @maxItems 26
   */
  input_read_us: [
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter
  ];
  /**
   * @minItems 26
   * @maxItems 26
   */
  pose_decode_us: [
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter
  ];
  /**
   * @minItems 26
   * @maxItems 26
   */
  pose_run_us: [
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter
  ];
  /**
   * @minItems 26
   * @maxItems 26
   */
  preprocess_us: [
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter
  ];
  /**
   * @minItems 26
   * @maxItems 26
   */
  round_trip_us: [
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter
  ];
  /**
   * @minItems 26
   * @maxItems 26
   */
  serialize_us: [
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter,
    Counter
  ];
  [k: string]: unknown;
}
