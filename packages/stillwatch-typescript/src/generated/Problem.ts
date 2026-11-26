// Generated from sw-protocol schemas. Regenerate with npm run generate.

export type ProblemCode =
  | "invalid_request"
  | "unauthorized"
  | "forbidden"
  | "not_found"
  | "revision_conflict"
  | "idempotency_conflict"
  | "invalid_transition"
  | "capacity_exceeded"
  | "cursor_expired"
  | "rate_limited"
  | "provider_unavailable"
  | "storage_unavailable"
  | "internal_error"
  | "unrecognized";

export interface Problem {
  code: ProblemCode;
  detail?: string | null;
  request_id?: string | null;
  status: number;
  title: string;
  type: string;
  [k: string]: unknown;
}
