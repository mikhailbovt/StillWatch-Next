import { createHash, timingSafeEqual, randomUUID } from "node:crypto";
import { open } from "node:fs/promises";
import { createServer } from "node:http";
import { deleteApp, initializeServerApp } from "firebase/app";
import { appCheckProvider } from "./app-check.js";
import { firebaseSdkVersion, maxRequestBytes, readConfiguration } from "./config.js";
import { decodePacket } from "./packet.js";
import { evaluate, makeAI } from "./generate.js";

const RESPONSE_LIMIT = 12 * 1024;
const DEVICE_WAIT_MS = 4_000;
const DEVICE_GRACE_MS = 8_000;
const browserPath = new URL("../dist/hybrid.js", import.meta.url);
const pagePath = new URL("../public/index.html", import.meta.url);
const stylePath = new URL("../public/worker.css", import.meta.url);

async function readFile(path, limit, privateFile = false) {
  const file = await open(path, "r");
  try {
    const info = await file.stat();
    if (!info.isFile() || info.size < 1 || info.size > limit
        || (privateFile && process.platform !== "win32" && (info.mode & 0o077) !== 0)) {
      throw new Error("Invalid configuration or credential file");
    }
    const bytes = Buffer.alloc(info.size + 1);
    const { bytesRead } = await file.read(bytes, 0, bytes.length, 0);
    if (bytesRead !== info.size) throw new Error("Configuration changed while reading");
    return bytes.subarray(0, bytesRead);
  } finally {
    await file.close();
  }
}

function credential(value) {
  if (typeof value !== "string" || !/^[a-f0-9]{64}$/.test(value)) throw new Error("Invalid worker credential");
  return createHash("sha256").update(value).digest();
}

async function readBody(req, limit) {
  const length = Number(req.headers["content-length"]);
  if (!Number.isSafeInteger(length) || length < 1 || length > limit) throw new Error("invalid_body");
  const chunks = [];
  let bytes = 0;
  for await (const chunk of req) {
    bytes += chunk.length;
    if (bytes > length) throw new Error("invalid_body");
    chunks.push(chunk);
  }
  if (bytes !== length) throw new Error("invalid_body");
  return Buffer.concat(chunks, bytes);
}

async function nodeCloud(config, appCheck, packet, signal) {
  const { token } = await appCheck(signal);
  const app = initializeServerApp(config.firebase, {
    appCheckToken: token,
    automaticDataCollectionEnabled: false,
  });
  try {
    return await evaluate(makeAI(app), config.model, packet, signal);
  } finally {
    await deleteApp(app);
  }
}

async function serve(path, port, coreTokenFile, appCheckFile, browserTokenFile) {
  if (!Number.isInteger(port) || port < 1024 || port > 65535) throw new Error("Invalid loopback port");
  const raw = JSON.parse((await readFile(path, 8192)).toString("utf8"));
  const config = readConfiguration(raw);
  if (process.env.STILLWATCH_FIREBASE_PRODUCTION === "1" && config.appCheck !== "token_file") {
    throw new Error("Production worker requires token_file mode");
  }
  const coreDigest = credential((await readFile(coreTokenFile, 128, true)).toString("ascii").trim());
  const browserDigest = config.mode === "hybrid_web"
    ? credential((await readFile(browserTokenFile, 128, true)).toString("ascii").trim())
    : null;
  if (browserDigest && timingSafeEqual(coreDigest, browserDigest)) throw new Error("Separate worker credentials are required");
  const { acquire: appCheck, credentialStatus } = appCheckProvider(config, appCheckFile, readFile);
  const host = "127.0.0.1:" + port;
  const origin = "http://" + host;
  const hybrid = config.mode === "hybrid_web";
  const page = hybrid ? await readFile(pagePath, 8192) : null;
  const style = hybrid ? await readFile(stylePath, 8192) : null;
  const script = hybrid ? await readFile(browserPath, 2 * 1024 * 1024) : null;
  let localState = "disconnected";
  let lastBrowserSeen = Number.NEGATIVE_INFINITY;
  let waiting = null;
  let job = null;
  let running = false;

  function authenticated(req, expected) {
    const auth = req.headers.authorization;
    return expected !== null && typeof auth === "string" && /^Bearer [a-f0-9]{64}$/.test(auth)
      && timingSafeEqual(expected, credential(auth.slice(7)));
  }

  function reply(res, status, body, contentType = "application/json") {
    res.writeHead(status, {
      "Content-Type": contentType,
      "Cache-Control": "no-store",
      "X-Content-Type-Options": "nosniff",
      "Referrer-Policy": "no-referrer",
      "X-Frame-Options": "DENY",
      // The Firebase Chrome adapter decodes an inline JPEG with fetch(data:).
      "Content-Security-Policy": "default-src 'self'; img-src 'self' blob:; script-src 'self' https://www.google.com https://www.gstatic.com https://www.recaptcha.net; connect-src 'self' data: https://*.googleapis.com https://www.google.com https://www.recaptcha.net; frame-src https://www.google.com https://www.recaptcha.net; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'",
    });
    res.end(contentType === "application/json" ? JSON.stringify(body) : body);
  }

  function browserReady() {
    return browserState() !== "disconnected";
  }

  function browserState() {
    return waiting || job?.claimed || performance.now() - lastBrowserSeen < DEVICE_GRACE_MS
      ? localState : "disconnected";
  }

  function dispatch() {
    if (!waiting || !job || job.claimed
        || (job.route === "local_only" && localState !== "ready")) return;
    job.claimed = true;
    waiting.finish(job);
  }

  function waitForJob(res) {
    if (res.destroyed) return;
    const clear = () => {
      clearTimeout(timer);
      if (waiting === poll) waiting = null;
    };
    const poll = { finish(current) {
      clear();
      res.removeListener("close", clear);
      if (res.destroyed) return;
      if (!current) return reply(res, 204, null);
      res.setHeader("X-StillWatch-Request", current.id);
      reply(res, 200, current.body, "application/octet-stream");
    } };
    const timer = setTimeout(() => poll.finish(null), DEVICE_WAIT_MS);
    waiting = poll;
    res.once("close", clear);
    dispatch();
  }

  async function request(req, res) {
    if (req.headers.host !== host || (req.headers.origin && req.headers.origin !== origin)) {
      return reply(res, 403, { error: "origin_denied" });
    }
    if (hybrid && req.method === "GET" && req.url === "/") return reply(res, 200, page, "text/html; charset=utf-8");
    if (hybrid && req.method === "GET" && req.url === "/worker.css") return reply(res, 200, style, "text/css; charset=utf-8");
    if (hybrid && req.method === "GET" && req.url === "/hybrid.js") return reply(res, 200, script, "text/javascript; charset=utf-8");
    if (hybrid && req.url?.startsWith("/device/")) {
      if (!authenticated(req, browserDigest)) return reply(res, 401, { error: "unauthorized" });
      if (req.method === "GET" && req.url === "/device/config") {
        return reply(res, 200, { firebase: config.firebase, model: config.model });
      }
      if (req.method === "POST" && req.url === "/device/poll") {
        const state = JSON.parse((await readBody(req, 128)).toString("utf8")).state;
        if (!["ready", "unavailable", "initializing"].includes(state)) return reply(res, 400, { error: "invalid_state" });
        if (waiting) return reply(res, 409, { error: "device_poll_active" });
        localState = state;
        lastBrowserSeen = performance.now();
        return waitForJob(res);
      }
      if (req.method === "POST" && req.url === "/device/result") {
        if (!job?.claimed || req.headers["x-stillwatch-request"] !== job.id) return reply(res, 409, { error: "expired" });
        const current = job;
        const body = JSON.parse((await readBody(req, RESPONSE_LIMIT)).toString("utf8"));
        if (current !== job || current.finished) return reply(res, 409, { error: "expired" });
        current.finished = true;
        lastBrowserSeen = performance.now();
        current.resolve(body);
        return reply(res, 204, null);
      }
      if (req.method === "POST" && req.url === "/device/attestation") {
        const current = job;
        if (!current?.claimed || current.finished || current.route === "local_only" || current.attestation
            || req.headers["x-stillwatch-request"] !== current.id) {
          return reply(res, 409, { error: "expired_or_ineligible" });
        }
        current.attestation = true;
        try {
          const token = await appCheck(AbortSignal.any([current.signal, AbortSignal.timeout(4_000)]));
          if (job !== current || current.finished || current.signal.aborted) {
            return reply(res, 409, { error: "expired_or_ineligible" });
          }
          return reply(res, 200, token);
        } catch {
          return reply(res, 503, { error: "app_check_unavailable" });
        }
      }
      return reply(res, 404, { error: "not_found" });
    }
    if (!authenticated(req, coreDigest)) return reply(res, 401, { error: "unauthorized" });
    if (req.method === "GET" && req.url === "/health") {
      const appCheckState = await credentialStatus();
      return reply(res, 200, {
        mode: config.mode, sdk_version: firebaseSdkVersion, model: config.model,
        semantic_evaluation: true,
        cloud_transport: appCheckState !== "unavailable" && (!hybrid || browserReady()),
        app_check_state: appCheckState,
        local_state: hybrid ? browserState() : "disconnected",
        max_images: 5, max_bytes: maxRequestBytes, busy: running,
      });
    }
    if (req.method !== "POST" || req.url !== "/evaluate") return reply(res, 404, { error: "not_found" });
    if (running) return reply(res, 429, { error: "busy" });
    if (req.headers["content-type"] !== "application/octet-stream") return reply(res, 415, { error: "invalid_body" });
    const requestId = req.headers["x-stillwatch-request-id"];
    if (requestId !== undefined && (typeof requestId !== "string" ||
        !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(requestId))) {
      return reply(res, 400, { error: "invalid_request_id" });
    }
    if (requestId) res.setHeader("X-StillWatch-Request-Id", requestId);
    running = true;
    let controller;
    let timer;
    try {
      const body = await readBody(req, maxRequestBytes);
      const packet = await decodePacket(body.buffer.slice(body.byteOffset, body.byteOffset + body.length));
      controller = new AbortController();
      const aborted = () => controller.abort();
      res.once("close", aborted);
      timer = setTimeout(aborted, packet.meta.timeout_ms);
      let result;
      if (!hybrid) {
        result = await nodeCloud(config, appCheck, packet, controller.signal);
      } else {
        if (!browserReady() || (packet.meta.route === "local_only" && localState !== "ready")) {
          return reply(res, 503, { error: "local_unavailable" });
        }
        result = await new Promise((resolve, reject) => {
          controller.signal.addEventListener("abort", () => reject(new Error("timeout")), { once: true });
          job = { id: requestId ?? randomUUID(), body, resolve, claimed: false, finished: false,
            route: packet.meta.route, attestation: false, signal: controller.signal };
          dispatch();
        });
        if (result?.error) return reply(res, 503, { error: "inference_unavailable" });
      }
      if (!res.destroyed) reply(res, 200, result);
    } catch (error) {
      if (!res.destroyed) {
        const status = controller?.signal.aborted ? 504 : error?.status === 429 ? 429 : 502;
        reply(res, status, { error: status === 504 ? "deadline" : status === 429 ? "rate_limited" : "inference_unavailable" });
      }
    } finally {
      clearTimeout(timer);
      job = null;
      running = false;
    }
  }

  const server = createServer({ requestTimeout: 20_000, headersTimeout: 3_000, maxHeaderSize: 4096 }, (req, res) => {
    request(req, res).catch(() => {
      if (!res.headersSent && !res.destroyed) reply(res, 400, { error: "bad_request" });
    });
  });
  server.maxConnections = 8;
  server.keepAliveTimeout = 2_000;
  server.setTimeout(20_000);
  server.on("error", () => {
    console.error("StillWatch Firebase worker could not bind its loopback listener.");
    process.exitCode = 1;
  });
  server.listen(port, "127.0.0.1");
  const shutdown = () => {
    server.close();
    waiting?.finish(null);
    setTimeout(() => server.closeAllConnections(), 1_500).unref();
  };
  process.once("SIGINT", shutdown);
  process.once("SIGTERM", shutdown);
}

const [, , config, port, coreToken, appCheck, browserToken] = process.argv;
serve(config, Number(port), coreToken, appCheck, browserToken).catch(() => {
  console.error("StillWatch Firebase worker failed to start. Check the loopback port, configuration, and private credential files.");
  process.exitCode = 1;
});
