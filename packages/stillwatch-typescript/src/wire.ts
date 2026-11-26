import type { Change } from "./generated/Change.js";

export class CoreError extends Error {
  constructor(readonly code: string, readonly status?: number, readonly retryAfter?: number) {
    super(status === undefined ? code : code + " (HTTP " + status + ")");
    this.name = "CoreError";
  }

  get reconnectable(): boolean {
    return this.code === "transport" || this.code === "deadline" ||
      (this.status !== undefined && [429, 502, 503, 504].includes(this.status));
  }
}

export function uuid(value: unknown): string {
  if (typeof value !== "string" ||
      value.length !== 36 || !/^[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}$/.test(value) ||
      value === "00000000-0000-0000-0000-000000000000") throw new CoreError("protocol");
  return value;
}

export function counter(value: unknown): string {
  if (typeof value !== "string" || !/^(0|[1-9][0-9]{0,19})(?![\s\S])/.test(value) ||
      BigInt(value) > (1n << 64n) - 1n) throw new CoreError("protocol");
  return value;
}

export class Cursor {
  readonly generation: string;
  readonly sequence: string;
  constructor(value: string) {
    const [generation, sequence, extra] = value.split(":");
    if (extra !== undefined) throw new CoreError("protocol");
    this.generation = uuid(generation);
    this.sequence = counter(sequence);
  }

  toString(): string {
    return this.generation + ":" + this.sequence;
  }
}

export interface ChangeEvent {
  cursor: Cursor;
  type: string;
  change?: Change;
}

export function origin(address: string): URL {
  try {
    const url = new URL(address);
    if (url.protocol === "https:" && url.hostname && url.pathname === "/" &&
        !url.username && !url.password && !/[\s\\?#@]/.test(address) && url.port !== "0") return url;
  } catch {
    // Invalid input is reported without echoing the address.
  }
  throw new CoreError("endpoint");
}

export function token(value: unknown): string {
  if (typeof value !== "string" || value.length !== 64 || !/^[a-f0-9]{64}$/.test(value)) throw new CoreError("credentials");
  return value;
}

export function object(value: unknown): Record<string, unknown> {
  if (value === null || Array.isArray(value) || typeof value !== "object") throw new CoreError("protocol");
  return value as Record<string, unknown>;
}

const utf8 = new TextDecoder("utf-8", { fatal: true });

export function decode(bytes: Uint8Array): string {
  try {
    return utf8.decode(bytes);
  } catch {
    throw new CoreError("protocol");
  }
}

export function json(bytes: Uint8Array): unknown {
  try {
    return JSON.parse(decode(bytes));
  } catch {
    throw new CoreError("protocol");
  }
}

export function responseError(status: number, data: Uint8Array, retryAfter?: number): CoreError {
  try {
    const problem = object(json(data));
    if (problem.status === status && typeof problem.code === "string" && /^[a-z_]{1,64}$/.test(problem.code)) {
      return new CoreError(problem.code, status, retryAfter);
    }
  } catch {
    // A proxy can respond with HTML. Preserve the status without the body.
  }
  return new CoreError("http", status, retryAfter);
}

export class SseParser {
  static readonly limit = 64 * 1024;
  readonly #line = new Uint8Array(SseParser.limit);
  #length = 0;
  #size = 0;
  #skipLf = false;
  #id = "";
  #event = "";
  #data: string[] = [];

  *feed(chunk: Uint8Array): Generator<ChangeEvent> {
    for (const byte of chunk) {
      if (this.#skipLf) {
        this.#skipLf = false;
        if (byte === 10) continue;
      }
      if (byte !== 10 && byte !== 13) {
        if (this.#length >= this.#line.length) throw new CoreError("protocol");
        this.#line[this.#length++] = byte;
        continue;
      }
      this.#skipLf = byte === 13;
      if (this.#length === 0) {
        const change = this.#finish();
        if (change) yield change;
        continue;
      }
      const bytes = this.#line.subarray(0, this.#length);
      this.#length = 0;
      if (bytes[0] === 58) continue;
      this.#size += bytes.length + 1;
      if (this.#size > SseParser.limit) throw new CoreError("protocol");
      const line = decode(bytes);
      const colon = line.indexOf(":");
      const field = colon === -1 ? line : line.slice(0, colon);
      const raw = colon === -1 ? "" : line.slice(colon + 1);
      const value = raw.startsWith(" ") ? raw.slice(1) : raw;
      if (field === "event") this.#event = value;
      if (field === "id" && !value.includes("\0")) this.#id = value;
      if (field === "data") this.#data.push(value);
    }
  }

  #finish(): ChangeEvent | undefined {
    const { id, event, data } = { id: this.#id, event: this.#event, data: this.#data };
    this.#id = this.#event = "";
    this.#data = [];
    this.#size = 0;
    if (data.length === 0) return;
    if (event === "resync") throw new CoreError("cursor_expired");
    const cursor = new Cursor(id);
    let payload: Record<string, unknown>;
    try {
      payload = object(JSON.parse(data.join("\n")));
    } catch {
      throw new CoreError("protocol");
    }
    if (payload.schema_version !== "1.0" || payload.type !== event ||
        payload.stream_generation !== cursor.generation || payload.sequence !== cursor.sequence) {
      throw new CoreError("protocol");
    }
    const entity = uuid(payload.entity_id);
    const revision = counter(payload.entity_revision);
    if (event !== "incident.opened" && event !== "incident.updated") return { cursor, type: event };
    const incident = object(payload.payload);
    if (incident.id !== entity || incident.revision !== revision) throw new CoreError("protocol");
    return { cursor, type: event, change: payload as unknown as Change };
  }
}
