import { initializeApp } from "firebase/app";
import { CustomProvider, initializeAppCheck } from "firebase/app-check";
import { bootstrapDevice, evaluate, makeAI } from "./generate.js";
import { decodePacket } from "./packet.js";

const status = document.querySelector("#status");
const key = document.querySelector("#key");
const connect = document.querySelector("#connect");
const keep = document.querySelector("#keep");
const disconnect = document.querySelector("#disconnect");
const prepare = document.querySelector("#prepare");
const savedCredential = "stillwatch.worker.credential";
let credential = "";
let reconnect;
let configured;
let ai;
let state = "unavailable";
let currentJob;

function request(path, options = {}) {
  const { headers, ...init } = options;
  return fetch(path, { ...init, cache: "no-store", credentials: "omit",
    headers: { ...headers, Authorization: "Bearer " + credential },
  });
}

connect.addEventListener("click", () => void connectBrowser(key.value.trim(), keep.checked));
disconnect.addEventListener("click", () => {
  rememberCredential(null);
  credential = "";
  window.location.reload();
});

function rememberCredential(value) {
  try {
    if (value === null) localStorage.removeItem(savedCredential);
    else localStorage.setItem(savedCredential, value);
    return true;
  } catch {
    return false;
  }
}

async function connectBrowser(value, remember) {
  if (!/^[a-f0-9]{64}$/.test(value)) {
    status.textContent = "Enter the 64-character worker credential.";
    return;
  }
  credential = value;
  connect.disabled = true;
  try {
    const result = await request("/device/config", { signal: AbortSignal.timeout(4_000) });
    if (result.status === 401) {
      rememberCredential(null);
      reconnect = undefined;
      credential = "";
      connect.disabled = false;
      disconnect.hidden = true;
      status.textContent = "The worker rejected this credential. Enter the current credential to connect.";
      return;
    }
    if (!result.ok) throw new Error("connection");
    configured = await result.json();
    const app = initializeApp(configured.firebase, { automaticDataCollectionEnabled: false });
    initializeAppCheck(app, {
      provider: new CustomProvider({ getToken: async () => {
        if (!currentJob) throw new Error("No active evaluation");
        const result = await request("/device/attestation", {
          method: "POST", headers: { "X-StillWatch-Request": currentJob },
          signal: AbortSignal.timeout(4_000),
        });
        if (!result.ok) throw new Error("App Check unavailable");
        return result.json();
      } }),
      isTokenAutoRefreshEnabled: false,
    });
    ai = makeAI(app);
    if (remember) {
      keep.checked = rememberCredential(credential);
      if (keep.checked) reconnect = credential;
    }
    key.value = "";
    key.disabled = true;
    keep.disabled = true;
    disconnect.hidden = false;
    prepare.disabled = false;
    status.textContent = "Connected. Initialize the Chrome model to enable local evaluation.";
    void poll();
    try {
      // A model previously downloaded in this profile can be ready without user activation.
      const available = await globalThis.LanguageModel?.availability({
        expectedInputs: [{ type: "text", languages: ["en"] }, { type: "image" }],
      });
      if (available === "available" && state === "unavailable") {
        state = "ready";
        status.textContent = "Chrome model available. Waiting for evidence.";
      }
    } catch {
      // The browser can still serve a consented cloud request.
    }
  } catch {
    credential = "";
    if (reconnect === value) {
      status.textContent = "Waiting for the local worker to start…";
      setTimeout(() => void connectBrowser(value, true), 2_000);
    } else {
      connect.disabled = false;
      status.textContent = "Connection failed. Check the credential and worker configuration.";
    }
  }
}

try {
  reconnect = localStorage.getItem(savedCredential);
} catch {
  keep.disabled = true;
}
if (reconnect && /^[a-f0-9]{64}$/.test(reconnect)) {
  keep.checked = true;
  disconnect.hidden = false;
  void connectBrowser(reconnect, true);
} else if (reconnect) {
  rememberCredential(null);
  reconnect = undefined;
}

prepare.addEventListener("click", async () => {
  prepare.disabled = true;
  state = "initializing";
  status.textContent = "Initializing Chrome model…";
  try {
    await bootstrapDevice(ai).initializeDeviceModel(progress => {
      status.textContent = "Downloading Chrome model (" + Math.round(100 * progress) + "%)";
    });
    state = "ready";
    status.textContent = "Chrome model initialized. Waiting for evidence.";
  } catch {
    state = "unavailable";
    status.textContent = "The Chrome model is unavailable. Review the browser requirements and retry.";
    prepare.disabled = false;
  }
});

async function poll() {
  if (!credential) return;
  try {
    const response = await request("/device/poll", {
      method: "POST", headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ state }), signal: AbortSignal.timeout(4_000),
    });
    if (response.status === 401) {
      rememberCredential(null);
      credential = "";
      window.location.reload();
      return;
    }
    if (response.status === 200) {
      const requestId = response.headers.get("X-StillWatch-Request");
      currentJob = requestId;
      let result;
      try {
        const packet = await decodePacket(await response.arrayBuffer());
        result = await evaluate(ai, configured.model, packet, AbortSignal.timeout(packet.meta.timeout_ms), true);
        status.textContent = result.inference_source === "on_device" ? "Last evaluation ran on this device." : "Last evaluation ran in the cloud.";
      } catch (error) {
        result = { error: "inference_unavailable" };
        const reason = evaluationFailure(error);
        status.textContent = `Evaluation unavailable (${reason}). Waiting for the next request.`;
      } finally {
        currentJob = undefined;
      }
      await request("/device/result", {
        method: "POST", body: JSON.stringify(result),
        headers: { "Content-Type": "application/json", "X-StillWatch-Request": requestId },
        signal: AbortSignal.timeout(4_000),
      });
    }
  } catch {
    status.textContent = "Connection interrupted. Reconnecting…";
  } finally {
    setTimeout(poll, 350);
  }
}

function evaluationFailure(error) {
  if (error instanceof SyntaxError || error?.message === "invalid_response") return "invalid response";
  if (error?.message === "blocked_response") return "blocked response";
  if (error?.message === "unverified_route") return "route unverified";
  if (error?.name === "TimeoutError" || error?.name === "AbortError") return "deadline";
  if (error?.name === "NotSupportedError" || error?.code === "api-not-enabled" || error?.code === "unsupported") return "unsupported device";
  if (error?.name === "NotAllowedError") return "browser permission";
  if (error?.name === "QuotaExceededError") return "model quota";
  if (error instanceof TypeError || error?.name === "InvalidStateError") return "browser runtime";
  return "provider error";
}
