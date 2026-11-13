import { createHash, createPrivateKey, createPublicKey, generateKeyPairSync, randomUUID, sign, X509Certificate } from "node:crypto";
import { checkServerIdentity } from "node:tls";
import { Agent, fetch as nativeFetch } from "undici";
import type { RequestInit as NativeInit } from "undici";

import { StillWatchClient, fetchResponse, interruptible, read } from "./index.js";
import type { Credentials, Fetch, PairingOffer } from "./index.js";
import type { DeviceCredentials } from "./generated/DeviceCredentials.js";
import { CoreError, json, object, origin, responseError, token, uuid } from "./wire.js";

export type { DeviceCredentials };

export interface EndpointPin {
  endpoint: string;
  core_instance_id: string;
  tls_certificate_sha256: string;
  tls_certificate_der: string;
}

export interface Enrollment {
  core: EndpointPin;
  seed: Uint8Array;
  credentials: DeviceCredentials;
}

function agent(pin: EndpointPin): Agent {
  const expected = pin.tls_certificate_sha256;
  const value = pin.tls_certificate_der;
  if (!/^[a-f0-9]{64}$/.test(expected) || !/^[A-Za-z0-9_-]{1,8192}$/.test(value)) {
    throw new CoreError("endpoint");
  }
  const der = Buffer.from(value, "base64url");
  if (createHash("sha256").update(der).digest("hex") !== expected) throw new CoreError("endpoint");
  let certificate: X509Certificate;
  try {
    certificate = new X509Certificate(der);
  } catch {
    throw new CoreError("endpoint");
  }
  return new Agent({
    connections: 8,
    pipelining: 1,
    connect: {
      ca: certificate.toString(),
      minVersion: "TLSv1.3",
      rejectUnauthorized: true,
      timeout: 3_000,
      checkServerIdentity: (name, peer) =>
        createHash("sha256").update(peer.raw).digest("hex") === expected
          ? checkServerIdentity(name, peer)
          : new Error("Core certificate changed"),
    },
  });
}

function transport(dispatcher: Agent): Fetch {
  return async (url, init) => nativeFetch(url, { ...init as NativeInit, dispatcher }) as unknown as Response;
}

export class NodeStillWatchClient extends StillWatchClient {
  readonly #agent: Agent;

  constructor(pin: EndpointPin, options: {
    seed?: Uint8Array;
    credentials?: DeviceCredentials;
    persist?: (value: DeviceCredentials) => Promise<void>;
  } = {}) {
    const endpoint = origin(pin.endpoint).origin;
    const instance = uuid(pin.core_instance_id);
    const dispatcher = agent(pin);
    const fetch = transport(dispatcher);
    let credentials: Credentials | undefined;
    try {
      if (options.seed !== undefined || options.credentials !== undefined) {
        if (!options.seed || !options.credentials || !options.persist) throw new CoreError("credentials");
        credentials = new DeviceSession(pin, fetch, options.seed, options.credentials, options.persist);
      }
    } catch (error) {
      void dispatcher.destroy();
      throw error;
    }
    super(endpoint, { fetch, instance, credentials });
    this.#agent = dispatcher;
  }

  override close(): void {
    super.close();
    void this.#agent.destroy();
  }
}

export async function enroll(offer: PairingOffer, deviceLabel: string, signal?: AbortSignal): Promise<Enrollment> {
  const label = deviceLabel.trim();
  if (offer.api?.major !== 2 || !label || Array.from(label).length > 80 || /[\x00-\x1f\x7f]/.test(label) ||
      !(Date.parse(offer.expires_at) > Date.now())) throw new CoreError("credentials");
  const secret = token(offer.secret);
  const challenge = uuid(offer.challenge);
  const core: EndpointPin = {
    endpoint: origin(offer.endpoint).origin,
    core_instance_id: uuid(offer.core_instance_id),
    tls_certificate_sha256: offer.tls_certificate_sha256,
    tls_certificate_der: offer.tls_certificate_der,
  };
  const dispatcher = agent(core);
  const fetch = transport(dispatcher);
  const client = new StillWatchClient(core.endpoint, { instance: core.core_instance_id, fetch });
  const { privateKey, publicKey } = generateKeyPairSync("ed25519");
  const seed = privateKey.export({ format: "der", type: "pkcs8" }).subarray(-32);
  const publicBytes = publicKey.export({ format: "der", type: "spki" }).subarray(-32);
  try {
    await client.core(signal);
    const credentials = await command(fetch, core.endpoint, "/api/v2/pairing/finish", {
      challenge, secret, label, public_key: publicBytes.toString("hex"),
      signature: sign(null, proof("pair", [core.core_instance_id, challenge], secret + label), privateKey).toString("hex"),
    }, signal);
    return { core, seed: Uint8Array.from(seed), credentials };
  } finally {
    client.close();
    await dispatcher.destroy();
  }
}

class DeviceSession implements Credentials {
  readonly #key;
  #value: DeviceCredentials;
  #flight?: Promise<void>;
  #terminal = false;
  #dirty = false;

  constructor(readonly core: EndpointPin, readonly fetch: Fetch, seed: Uint8Array,
              value: DeviceCredentials, readonly persist: (value: DeviceCredentials) => Promise<void>) {
    if (seed.length !== 32) throw new CoreError("credentials");
    const prefix = Buffer.from("302e020100300506032b657004220420", "hex");
    this.#key = createPrivateKey({ key: Buffer.concat([prefix, seed]), format: "der", type: "pkcs8" });
    if (createPublicKey(this.#key).asymmetricKeyType !== "ed25519") throw new CoreError("credentials");
    this.#value = validate(value);
  }

  async access(signal: AbortSignal, rejected?: string): Promise<string> {
    if (this.#flight) await interruptible(this.#flight, signal);
    if (this.#terminal) throw new CoreError("credentials");
    const refresh = Date.parse(this.#value.access_expires_at) <= Date.now() + 15_000 ||
      rejected === this.#value.access_token;
    if (refresh || this.#dirty) {
      const work = this.#update(refresh);
      const flight = work.finally(() => { this.#flight = undefined; });
      this.#flight = flight;
      await interruptible(flight, signal);
    }
    return this.#value.access_token;
  }

  async #update(refresh: boolean): Promise<void> {
    if (refresh) {
      if (Date.parse(this.#value.refresh_expires_at) <= Date.now()) throw new CoreError("credentials");
      const device = this.#value.device.id;
      const refreshToken = this.#value.refresh_token;
      const nonce = randomUUID();
      const message = proof("refresh", [this.core.core_instance_id, device, nonce], refreshToken);
      // A lost response may have consumed the token. Do not automatically reuse it.
      this.#terminal = true;
      const updated = await command(this.fetch, this.core.endpoint, "/api/v2/device-session/refresh", {
        device_id: device, nonce, refresh_token: refreshToken,
        signature: sign(null, message, this.#key).toString("hex"),
      });
      if (updated.device.id !== device) throw new CoreError("identity");
      this.#value = updated;
      this.#dirty = true;
      this.#terminal = false;
    }
    if (this.#dirty) {
      await this.persist(structuredClone(this.#value));
      this.#dirty = false;
    }
  }
}

function proof(purpose: "pair" | "refresh", ids: string[], suffix: string): Buffer {
  return Buffer.concat([
    Buffer.from("stillwatch/" + purpose + "/v2\0"),
    ...ids.map(id => Buffer.from(uuid(id).replaceAll("-", ""), "hex")),
    Buffer.from(suffix, "utf8"),
  ]);
}

async function command(fetch: Fetch, endpoint: string, path: string, body: unknown,
                       parent?: AbortSignal): Promise<DeviceCredentials> {
  const timeout = AbortSignal.timeout(12_000);
  const signal = parent ? AbortSignal.any([parent, timeout]) : timeout;
  const response = await fetchResponse(fetch, new URL(path, endpoint).href, {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body),
  }, signal);
  const data = await read(response, 16 * 1024, signal);
  if (!response.ok) throw responseError(response.status, data);
  return validate(json(data));
}

function validate(value: unknown): DeviceCredentials {
  const response = object(value);
  const device = object(response.device);
  uuid(device.id);
  token(response.access_token);
  token(response.refresh_token);
  if (device.revoked_at != null || typeof response.access_expires_at !== "string" ||
      typeof response.refresh_expires_at !== "string" ||
      !(Date.parse(response.refresh_expires_at) > Date.parse(response.access_expires_at))) {
    throw new CoreError("credentials");
  }
  return structuredClone(response) as unknown as DeviceCredentials;
}
