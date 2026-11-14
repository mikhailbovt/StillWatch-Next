import type { AlertFeed } from "./generated/AlertFeed.js";
import type { AnalysisStatus } from "./generated/AnalysisStatus.js";
import type { Bootstrap } from "./generated/Bootstrap.js";
import type { Camera } from "./generated/Camera.js";
import type { CameraConfig } from "./generated/CameraConfig.js";
import type { CameraPage } from "./generated/CameraPage.js";
import type { CameraPatch } from "./generated/CameraPatch.js";
import type { ArrayOf_CaptureDevice } from "./generated/CaptureDevices.js";
import type { ArrayOf_Device } from "./generated/Devices.js";
import type { EvidencePacket } from "./generated/EvidencePacket.js";
import type { Health } from "./generated/Health.js";
import type { HealthEvent, HealthEventPage } from "./generated/HealthEventPage.js";
import type { Incident } from "./generated/Incident.js";
import type { IncidentPage } from "./generated/IncidentPage.js";
import type { ArrayOf_OnvifDevice } from "./generated/OnvifDevices.js";
import type { ArrayOf_OnvifProfile } from "./generated/OnvifProfiles.js";
import type { OnvifProfilesRequest } from "./generated/OnvifProfilesRequest.js";
import type { OnvifStream } from "./generated/OnvifStream.js";
import type { OnvifStreamRequest } from "./generated/OnvifStreamRequest.js";
import type { PairingOffer } from "./generated/PairingOffer.js";
import type { PairingRequest } from "./generated/PairingRequest.js";
import type { SemanticStatus } from "./generated/SemanticStatus.js";
import type { Snapshot } from "./generated/Snapshot.js";
import type { SourceProbe } from "./generated/SourceProbe.js";
import type { SourceProbeRequest } from "./generated/SourceProbeRequest.js";
import type { StreamGrant } from "./generated/StreamGrant.js";
import type { StreamRequest } from "./generated/StreamRequest.js";
import type { ReplayUpload } from "./generated/ReplayUpload.js";
import { CoreError, Cursor, SseParser, counter, decode, json, object, origin, responseError, token, uuid } from "./wire.js";
import type { ChangeEvent } from "./wire.js";
import type { CoreStatus } from "./generated/CoreStatus.js";

export type { Bootstrap, Camera, CameraConfig, CameraPatch, CoreStatus, EvidencePacket, Incident, PairingOffer, PairingRequest, ReplayUpload, Snapshot, StreamGrant };
export type { ArrayOf_OnvifDevice, ArrayOf_OnvifProfile, OnvifProfilesRequest, OnvifStream, OnvifStreamRequest };
export type { ChangeEvent };
export type { HealthEvent, HealthEventPage };
export { CoreError, Cursor, counter };

export interface Credentials {
  access(signal: AbortSignal, rejected?: string): Promise<string>;
}

export type Fetch = (url: string, init: RequestInit) => Promise<Response>;
type JsonOptions = RequestOptions & { json?: unknown };
type RequestOptions = {
  authorized?: boolean;
  signal?: AbortSignal;
  headers?: Record<string, string>;
  content?: BodyInit;
  key?: string;
  revision?: string;
  limit?: number;
  timeoutMs?: number;
};
type ResponseData = { headers: Headers; data: Uint8Array };

const MAX_JSON = 4 * 1024 * 1024;
const REQUEST_MS = 12_000;
const CAMERA_SETUP_MS = 22_000;
const WHEP_OFFER_MS = 28_000;

export class StillWatchClient {
  readonly #url: URL;
  readonly #fetch: Fetch;
  readonly #credentials?: Credentials;
  readonly #instance?: string;
  readonly #closed = new AbortController();

  /** A browser uses system HTTPS trust. Pairing a self-signed Core uses the Node entry point. */
  constructor(endpoint: string, options: { credentials?: Credentials; fetch?: Fetch; instance?: string } = {}) {
    this.#url = origin(endpoint);
    this.#fetch = options.fetch ?? globalThis.fetch.bind(globalThis);
    this.#credentials = options.credentials;
    this.#instance = options.instance === undefined ? undefined : uuid(options.instance);
  }

  async core(signal?: AbortSignal): Promise<Bootstrap> {
    const result = await this.#json<Bootstrap>("GET", "/bootstrap/info", { signal, authorized: false });
    const identity = uuid(result.instance_id);
    if (result.version?.major !== 2 || (this.#instance !== undefined && identity !== this.#instance)) {
      throw new CoreError("identity");
    }
    return result;
  }

  health(signal?: AbortSignal): Promise<Health> {
    return this.#json("GET", "/health/ready", { signal });
  }

  status(signal?: AbortSignal): Promise<CoreStatus> {
    return this.#json("GET", "/api/v2/status", { signal });
  }

  semanticStatus(signal?: AbortSignal): Promise<SemanticStatus> {
    return this.#json("GET", "/api/v2/semantic-status", { signal });
  }

  healthEvents(options: { before?: string; limit?: number; activeOnly?: boolean; signal?: AbortSignal } = {}): Promise<HealthEventPage> {
    const query = new URLSearchParams({ limit: String(options.limit ?? 100) });
    if (options.before !== undefined) query.set("before", counter(options.before));
    if (options.activeOnly) query.set("active_only", "true");
    return this.#json("GET", "/api/v2/health-events?" + query, { signal: options.signal });
  }

  cameras(after?: string, limit = 100, signal?: AbortSignal): Promise<CameraPage> {
    return this.#json("GET", page("/api/v2/cameras", after === undefined ? undefined : uuid(after), limit), { signal });
  }

  camera(id: string, signal?: AbortSignal): Promise<Camera> {
    return this.#json("GET", "/api/v2/cameras/" + uuid(id), { signal });
  }

  analysis(id: string, signal?: AbortSignal): Promise<AnalysisStatus> {
    return this.#json("GET", "/api/v2/cameras/" + uuid(id) + "/analysis-status", { signal });
  }

  probe(request: SourceProbeRequest, signal?: AbortSignal): Promise<SourceProbe> {
    return this.#json("POST", "/api/v2/source-probes", { json: request, timeoutMs: CAMERA_SETUP_MS, signal });
  }

  async captureDevices(signal?: AbortSignal): Promise<ArrayOf_CaptureDevice> {
    return this.#list("/api/v2/capture-devices", signal);
  }

  discoverOnvif(signal?: AbortSignal): Promise<ArrayOf_OnvifDevice> {
    return this.#json("POST", "/api/v2/onvif/discovery", { signal });
  }

  onvifProfiles(request: OnvifProfilesRequest, signal?: AbortSignal): Promise<ArrayOf_OnvifProfile> {
    return this.#json("POST", "/api/v2/onvif/profiles", { json: request, signal });
  }

  onvifStream(request: OnvifStreamRequest, signal?: AbortSignal): Promise<OnvifStream> {
    return this.#json("POST", "/api/v2/onvif/stream", { json: request, signal });
  }

  async uploadReplay(video: Blob, signal?: AbortSignal): Promise<ReplayUpload> {
    if (video.size === 0 || video.size > 128 * 1024 * 1024) throw new CoreError("invalid_request");
    const { data } = await this.#request("POST", "/api/v2/replay-uploads", {
      headers: { "Content-Type": "application/octet-stream" }, content: video,
      limit: 16 * 1024, timeoutMs: 125_000, signal,
    });
    return json(data) as unknown as ReplayUpload;
  }

  createCamera(config: CameraConfig, key: string, signal?: AbortSignal): Promise<Camera> {
    return this.#json("POST", "/api/v2/cameras", { json: config, key, timeoutMs: CAMERA_SETUP_MS, signal });
  }

  patchCamera(id: string, patch: CameraPatch, key: string, revision: string, signal?: AbortSignal): Promise<Camera> {
    return this.#json("PATCH", "/api/v2/cameras/" + uuid(id), {
      json: patch, key, revision, timeoutMs: CAMERA_SETUP_MS, signal,
    });
  }

  async deleteCamera(id: string, key: string, revision: string, signal?: AbortSignal): Promise<void> {
    await this.#request("DELETE", "/api/v2/cameras/" + uuid(id), { key, revision, signal });
  }

  async snapshot(signal?: AbortSignal): Promise<Snapshot> {
    const value = await this.#json<Snapshot>("GET", "/api/v2/snapshot", { signal });
    new Cursor(value.cursor);
    return value;
  }

  alerts(signal?: AbortSignal): Promise<AlertFeed> {
    return this.#json("GET", "/api/v2/alerts", { signal });
  }

  alertPage(after?: string, limit = 128, signal?: AbortSignal): Promise<AlertFeed> {
    return this.#json("GET", page("/api/v2/alerts", after, limit), { signal });
  }

  async alertSeen(id: string, cycle: string, signal?: AbortSignal): Promise<void> {
    await this.#request("POST", "/api/v2/alerts/" + counter(id) + "/seen", {
      signal, headers: { "Content-Type": "application/json" },
      content: JSON.stringify({ attention_cycle: counter(cycle) }),
    });
  }

  incidents(after?: string, limit = 100, signal?: AbortSignal): Promise<IncidentPage> {
    return this.#json("GET", page("/api/v2/incidents", after, limit), { signal });
  }

  incident(id: string, signal?: AbortSignal): Promise<Incident> {
    return this.#json("GET", "/api/v2/incidents/" + uuid(id), { signal });
  }

  acknowledge(id: string, key: string, revision: string, signal?: AbortSignal): Promise<Incident> {
    return this.#incidentCommand("incidents/" + uuid(id) + "/acknowledgements", {}, key, revision, signal);
  }

  resolve(id: string, reason: string, key: string, revision: string, signal?: AbortSignal): Promise<Incident> {
    return this.#incidentCommand("incidents/" + uuid(id) + "/resolution", { reason }, key, revision, signal);
  }

  feedback(id: string, classification: "confirmed" | "false_alarm", reason: string,
           evidenceVersion: string, key: string, revision: string, signal?: AbortSignal): Promise<Incident> {
    return this.#incidentCommand("incidents/" + uuid(id) + "/feedback", {
      classification, reason, evidence_version: counter(evidenceVersion),
    }, key, revision, signal);
  }

  silence(id: string, seconds: number, key: string, revision: string, signal?: AbortSignal): Promise<Incident> {
    return this.#incidentCommand("alerts/" + uuid(id) + "/silence", { duration_seconds: seconds }, key, revision, signal);
  }

  #incidentCommand(path: string, body: unknown, key: string, revision: string, signal?: AbortSignal): Promise<Incident> {
    return this.#json("POST", "/api/v2/" + path, { json: body, key, revision, signal });
  }

  evidence(id: string, packet: string, signal?: AbortSignal): Promise<EvidencePacket> {
    return this.#json("GET", evidencePath(id, packet), { signal });
  }

  evidenceImage(id: string, packet: string, ordinal: number, signal?: AbortSignal): Promise<Uint8Array> {
    if (!Number.isInteger(ordinal) || ordinal < 0 || ordinal > 255) throw new CoreError("invalid_request");
    return this.#image(evidencePath(id, packet) + "/" + ordinal, 512 * 1024, signal);
  }

  cameraImage(id: string, signal?: AbortSignal): Promise<Uint8Array> {
    return this.#image("/api/v2/cameras/" + uuid(id) + "/snapshot.jpg", 1024 * 1024, signal);
  }

  async #image(path: string, limit: number, signal?: AbortSignal): Promise<Uint8Array> {
    const { headers, data } = await this.#request("GET", path, { limit, signal });
    if (headers.get("content-type") !== "image/jpeg" || data[0] !== 0xff || data[1] !== 0xd8 || data[2] !== 0xff) {
      throw new CoreError("protocol");
    }
    return data;
  }

  devices(signal?: AbortSignal): Promise<ArrayOf_Device> {
    return this.#list("/api/v2/devices", signal);
  }

  issuePairing(request: PairingRequest, signal?: AbortSignal): Promise<PairingOffer> {
    return this.#json("POST", "/api/v2/pairing", { json: request, signal });
  }

  issueProxyPairing(request: PairingRequest, signal?: AbortSignal): Promise<PairingOffer> {
    return this.#json("POST", "/api/v2/pairing?profile=proxy", { json: request, signal });
  }

  async revokeDevice(id: string, signal?: AbortSignal): Promise<void> {
    await this.#request("DELETE", "/api/v2/devices/" + uuid(id), { signal });
  }

  async streamSession(request: StreamRequest, signal?: AbortSignal): Promise<StreamGrant> {
    const result = await this.#json<StreamGrant>("POST", "/api/v2/stream-sessions", { json: request, signal });
    if (result.whep_path !== whep(result.id) || result.codec !== "h264" || result.redacted !== true) {
      throw new CoreError("protocol");
    }
    return result;
  }

  async offerWhep(id: string, sdp: string, signal?: AbortSignal): Promise<{ sdp: string; etag: string | null }> {
    const path = whep(id);
    const { data, headers } = await this.#request("POST", path, {
      headers: { "Content-Type": "application/sdp" }, content: sdp, signal, limit: 48 * 1024, timeoutMs: WHEP_OFFER_MS,
    });
    if (headers.get("location") !== path || headers.get("content-type") !== "application/sdp") {
      throw new CoreError("protocol");
    }
    return { sdp: decode(data), etag: headers.get("etag") };
  }

  async trickle(id: string, etag: string, fragment: string, signal?: AbortSignal): Promise<void> {
    await this.#request("PATCH", whep(id), {
      headers: { "Content-Type": "application/trickle-ice-sdpfrag", "If-Match": etag }, content: fragment, signal,
    });
  }

  async closeWhep(id: string, signal?: AbortSignal): Promise<void> {
    await this.#request("DELETE", whep(id), { signal });
  }

  async #list<T>(path: string, signal?: AbortSignal): Promise<T> {
    const data = json((await this.#request("GET", path, { signal })).data);
    if (!Array.isArray(data)) throw new CoreError("protocol");
    return data as T;
  }

  async #json<T>(method: string, path: string, options: JsonOptions): Promise<T> {
    const headers = new Headers(options.headers);
    let content: string | undefined;
    if (options.json !== undefined) {
      headers.set("Content-Type", "application/json");
      content = JSON.stringify(options.json);
    }
    const result = await this.#request(method, path, { ...options, content, headers: Object.fromEntries(headers) });
    return object(json(result.data)) as T;
  }

  #signal(signal?: AbortSignal): AbortSignal {
    return signal ? AbortSignal.any([this.#closed.signal, signal]) : this.#closed.signal;
  }

  async #access(signal: AbortSignal, rejected?: string): Promise<string> {
    if (!this.#credentials) throw new CoreError("credentials");
    return token(await interruptible(this.#credentials.access(signal, rejected), signal));
  }

  async #send(path: string, init: RequestInit, signal: AbortSignal): Promise<Response> {
    const url = new URL(path, this.#url);
    if (!path.startsWith("/") || path.startsWith("//") || url.origin !== this.#url.origin) throw new CoreError("endpoint");
    return fetchResponse(this.#fetch, url.href, init, signal);
  }

  async #request(method: string, path: string, options: RequestOptions): Promise<ResponseData> {
    const started = performance.now();
    const timeoutMs = options.timeoutMs ?? REQUEST_MS;
    const signal = AbortSignal.any([this.#signal(options.signal), AbortSignal.timeout(timeoutMs)]);
    const headers = new Headers(options.headers);
    const repeatable = method === "GET" || options.key !== undefined;
    if (options.key !== undefined) {
      if (!/^[A-Za-z0-9_.-]{1,128}$/.test(options.key)) throw new CoreError("invalid_request");
      headers.set("Idempotency-Key", options.key);
    }
    if (options.revision !== undefined) headers.set("If-Match", '"' + counter(options.revision) + '"');
    let access = options.authorized === false ? undefined : await this.#access(signal);
    let renewed = false;
    for (let attempt = 0; attempt < 3; attempt++) {
      if (access !== undefined) headers.set("Authorization", "Bearer " + access);
      let response: Response;
      try {
        response = await this.#send(path, { method, headers, body: options.content }, signal);
      } catch (error) {
        if (!(error instanceof CoreError) || error.code !== "transport" || !repeatable || attempt !== 0) throw error;
        await sleep(150, signal);
        continue;
      }
      if (response.ok) return { headers: response.headers, data: await read(response, options.limit ?? MAX_JSON, signal) };
      const delay = retryAfter(response, attempt);
      const error = responseError(response.status, await read(response, 16 * 1024, signal), delay);
      if (response.status === 401 && access !== undefined && repeatable && !renewed && attempt < 2) {
        const replacement = await this.#access(signal, access);
        if (replacement === access) throw error;
        access = replacement;
        renewed = true;
        continue;
      }
      if (!repeatable || !error.reconnectable || attempt === 2 || performance.now() - started + delay >= timeoutMs) throw error;
      await sleep(delay, signal);
    }
    throw new CoreError("deadline");
  }

  async *changes(initial?: Cursor, externalSignal?: AbortSignal): AsyncGenerator<ChangeEvent> {
    const overall = this.#signal(externalSignal);
    let cursor = initial;
    let failures = 0;
    while (!overall.aborted) {
      let delay: number | undefined;
      const opening = new AbortController();
      const openingTimer = setTimeout(() => opening.abort(new DOMException("deadline", "TimeoutError")), REQUEST_MS);
      const signal = AbortSignal.any([overall, opening.signal]);
      try {
        let access = await this.#access(signal);
        const headers = new Headers({ Accept: "text/event-stream" });
        if (cursor) headers.set("Last-Event-ID", cursor.toString());
        headers.set("Authorization", "Bearer " + access);
        let response = await this.#send("/api/v2/changes", { method: "GET", headers }, signal);
        if (response.status === 401) {
          await response.body?.cancel();
          const replacement = await this.#access(signal, access);
          if (replacement === access) throw new CoreError("unauthorized", 401);
          access = replacement;
          headers.set("Authorization", "Bearer " + access);
          response = await this.#send("/api/v2/changes", { method: "GET", headers }, signal);
        }
        if (!response.ok) {
          delay = retryAfter(response, failures);
          throw responseError(response.status, await read(response, 16 * 1024, signal), delay);
        }
        clearTimeout(openingTimer);
        if (response.headers.get("content-type")?.split(";", 1)[0] !== "text/event-stream" || !response.body) {
          await response.body?.cancel();
          throw new CoreError("protocol");
        }
        const reader = response.body.getReader();
        const parser = new SseParser();
        try {
          while (!overall.aborted) {
            const chunk = await interruptible(reader.read(), AbortSignal.any([overall, AbortSignal.timeout(25_000)]));
            if (chunk.done) break;
            for (const event of parser.feed(chunk.value)) {
              if (cursor && cursor.generation !== event.cursor.generation) throw new CoreError("cursor_expired");
              if (cursor && BigInt(event.cursor.sequence) <= BigInt(cursor.sequence)) continue;
              yield event;
              cursor = event.cursor;
              failures = 0;
            }
          }
        } finally {
          await reader.cancel().catch(() => {});
        }
      } catch (error) {
        if (overall.aborted) return;
        if (error instanceof CoreError && !error.reconnectable) throw error;
      } finally {
        clearTimeout(openingTimer);
      }
      failures = Math.min(5, failures + 1);
      try {
        await sleep(delay ?? ((200 << failures) + Math.random() * 200), overall);
      } catch (error) {
        if (overall.aborted) return;
        throw error;
      }
    }
  }

  close(): void {
    this.#closed.abort();
  }
}

export async function fetchResponse(fetch: Fetch, url: string, init: RequestInit, signal: AbortSignal): Promise<Response> {
  try {
    return await fetch(url, { ...init, redirect: "manual", cache: "no-store", credentials: "omit", signal });
  } catch {
    if (signal.aborted) throw abortError(signal);
    throw new CoreError("transport");
  }
}

export async function read(response: Response, limit: number, signal: AbortSignal): Promise<Uint8Array> {
  const reader = response.body?.getReader();
  if (!reader) return new Uint8Array();
  const chunks: Uint8Array[] = [];
  let length = 0;
  try {
    if (Number(response.headers.get("content-length")) > limit) throw new CoreError("protocol");
    while (true) {
      const part = await interruptible(reader.read(), signal);
      if (part.done) break;
      length += part.value.length;
      if (length > limit) throw new CoreError("protocol");
      chunks.push(part.value);
    }
    const result = new Uint8Array(length);
    let offset = 0;
    for (const chunk of chunks) {
      result.set(chunk, offset);
      offset += chunk.length;
    }
    return result;
  } catch (error) {
    if (error instanceof CoreError) throw error;
    throw new CoreError("transport");
  } finally {
    await reader.cancel().catch(() => {});
  }
}

function abortError(signal: AbortSignal): CoreError {
  return new CoreError(signal.reason?.name === "TimeoutError" ? "deadline" : "cancelled");
}

export async function interruptible<T>(work: Promise<T>, signal: AbortSignal): Promise<T> {
  let listener: (() => void) | undefined;
  const aborted = new Promise<never>((_, reject) => {
    listener = () => reject(abortError(signal));
    if (signal.aborted) listener();
    else signal.addEventListener("abort", listener, { once: true });
  });
  try {
    return await Promise.race([work, aborted]);
  } finally {
    if (listener) signal.removeEventListener("abort", listener);
  }
}

async function sleep(milliseconds: number, signal: AbortSignal): Promise<void> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    await interruptible(new Promise<void>(resolve => { timer = setTimeout(resolve, milliseconds); }), signal);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

function retryAfter(response: Response, attempt: number): number {
  const header = response.headers.get("retry-after");
  if (header !== null) {
    if (/^\d+$/.test(header)) return Math.min(3_600_000, Number(header) * 1000);
    const date = Date.parse(header);
    if (Number.isFinite(date)) return Math.min(3_600_000, Math.max(0, date - Date.now()));
  }
  return 150 << Math.min(attempt, 5);
}

function page(path: string, after: string | undefined, limit: number): string {
  const query = new URLSearchParams({ limit: String(limit) });
  if (after !== undefined) query.set("after", after);
  return path + "?" + query;
}

function evidencePath(id: string, packet: string): string {
  return "/api/v2/incidents/" + uuid(id) + "/evidence/" + uuid(packet);
}

function whep(id: string): string {
  return "/api/v2/stream-sessions/" + uuid(id) + "/whep";
}
