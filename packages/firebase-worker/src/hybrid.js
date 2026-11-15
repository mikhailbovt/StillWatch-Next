import { initializeApp } from "firebase/app";
import { CustomProvider, initializeAppCheck } from "firebase/app-check";
import { bootstrapDevice, deviceAvailability, evaluate, makeAI, warmDevice } from "./generate.js";
import { decodePacket } from "./packet.js";

const status = document.querySelector("#status");
const key = document.querySelector("#key");
const connect = document.querySelector("#connect");
const keep = document.querySelector("#keep");
const disconnect = document.querySelector("#disconnect");
const prepare = document.querySelector("#prepare");
const evaluation = document.querySelector("#evaluation");
const incident = document.querySelector("#evaluation-incident");
const evidenceVersion = document.querySelector("#evaluation-version");
const requestedRoute = document.querySelector("#evaluation-requested");
const executionRoute = document.querySelector("#evaluation-route");
const evaluationModel = document.querySelector("#evaluation-model");
const evaluationClassification = document.querySelector("#evaluation-classification");
const evaluationResult = document.querySelector("#evaluation-result");
const evidence = document.querySelector("#evaluation-evidence");
const frame = document.querySelector("#evaluation-frame");
const frameTime = document.querySelector("#evaluation-frame-time");
const routeNames = { cloud_only: "Cloud only", local_only: "On-device only", prefer_local: "Prefer on-device" };
const savedCredential = "stillwatch.worker.credential";
let credential = "";
let reconnect;
let configured;
let ai;
let state = "unavailable";
let nextWarmup = 0;
let currentJob;
let evidenceUrl;
let evidenceTimer;

function request(path, options = {}) {
  const { headers, ...init } = options;
  return fetch(path, { ...init, cache: "no-store", credentials: "omit",
    headers: { ...headers, Authorization: "Bearer " + credential },
  });
}

connect.addEventListener("click", () => void connectBrowser(key.value.trim(), keep.checked));
disconnect.addEventListener("click", () => {
  hideEvidence();
  rememberCredential(null);
  credential = "";
  window.location.reload();
});

function hideEvidence() {
  clearTimeout(evidenceTimer);
  evidence.hidden = true;
  frame.removeAttribute("src");
  if (evidenceUrl) URL.revokeObjectURL(evidenceUrl);
  evidenceUrl = undefined;
}

document.addEventListener("visibilitychange", () => {
  if (document.visibilityState !== "visible") hideEvidence();
});
window.addEventListener("pagehide", hideEvidence);
document.addEventListener("freeze", hideEvidence);

function showPacket(packet) {
  const { meta } = packet;
  incident.textContent = meta.incident_id;
  evidenceVersion.textContent = meta.evidence_version;
  requestedRoute.textContent = routeNames[meta.route];
  executionRoute.textContent = "Waiting for the provider";
  evaluationModel.textContent = "Pending";
  evaluationClassification.textContent = "Pending";
  evaluationResult.textContent = "";
  evaluation.hidden = false;
  if (document.visibilityState !== "visible") return;
  const overview = packet.images.findLast(image => image.kind === "overview");
  evidenceUrl = URL.createObjectURL(new Blob([overview.jpeg], { type: "image/jpeg" }));
  frame.src = evidenceUrl;
  frameTime.textContent = `Masked overview ${overview.id} at ${overview.source_s.toFixed(2)} s`;
  evidence.hidden = false;
  evidenceTimer = setTimeout(hideEvidence, meta.timeout_ms);
}

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
    await warmDevice();
    state = "ready";
    status.textContent = "Chrome model ready. Waiting for evidence.";
  } catch {
    state = "unavailable";
    nextWarmup = performance.now() + 30_000;
    status.textContent = "The Chrome model is unavailable. Review the browser requirements and retry.";
    prepare.disabled = false;
  }
});

async function refreshDeviceAvailability() {
  if (state !== "unavailable" || performance.now() < nextWarmup) return;
  let available;
  try {
    // Chrome can discover a cached model after the worker has already connected.
    available = await deviceAvailability();
  } catch {
    return;
  }
  if (available !== "available" || state !== "unavailable") return;
  state = "initializing";
  prepare.disabled = true;
  status.textContent = "Warming the Chrome model…";
  try {
    await warmDevice();
    state = "ready";
    status.textContent = "Chrome model ready. Waiting for evidence.";
  } catch (error) {
    state = "unavailable";
    nextWarmup = performance.now() + 30_000;
    prepare.disabled = false;
    status.textContent = `Chrome model warmup failed (${evaluationFailure(error)}). Retrying after a delay.`;
  }
}

async function poll() {
  let reconnecting = false;
  // Fetch completion drives the healthy connection. A hidden tab may
  // otherwise run recurring JavaScript timers much less often.
  while (credential) {
    try {
      void refreshDeviceAvailability();
      const response = await request("/device/poll", {
        method: "POST", headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ state }), signal: AbortSignal.timeout(7_000),
      });
      if (response.status === 401) {
        rememberCredential(null);
        credential = "";
        window.location.reload();
        return;
      }
      if (response.status !== 200 && response.status !== 204) throw new Error("connection");
      if (reconnecting) {
        reconnecting = false;
        status.textContent = "Worker connected. Waiting for evidence.";
      }
      if (response.status === 200) await serveJob(response);
    } catch {
      reconnecting = true;
      hideEvidence();
      evaluation.hidden = true;
      status.textContent = "Connection interrupted. Reconnecting…";
      await new Promise(resolve => setTimeout(resolve, 1_000));
    }
  }
}

async function serveJob(response) {
  const requestId = response.headers.get("X-StillWatch-Request");
  currentJob = requestId;
  hideEvidence();
  evaluation.hidden = true;
  let result;
  let failure;
  try {
    const packet = await decodePacket(await response.arrayBuffer());
    showPacket(packet);
    result = await evaluate(ai, configured.model, packet, AbortSignal.timeout(packet.meta.timeout_ms),
      { browser: true, deviceReady: state === "ready" });
  } catch (error) {
    result = { error: "inference_unavailable" };
    failure = evaluationFailure(error);
    hideEvidence();
  } finally {
    currentJob = undefined;
  }
  const submitted = await request("/device/result", {
    method: "POST", body: JSON.stringify(result),
    headers: { "Content-Type": "application/json", "X-StillWatch-Request": requestId },
    signal: AbortSignal.timeout(4_000),
  });
  if (submitted.status === 409) {
    hideEvidence();
    evaluation.hidden = true;
    status.textContent = "The worker discarded the result after its deadline.";
    return;
  }
  if (!submitted.ok) throw new Error("result_submission");
  if (failure) {
    executionRoute.textContent = "Unavailable";
    evaluationModel.textContent = "Unavailable";
    evaluationClassification.textContent = "Unavailable";
    evaluationResult.textContent = `Evaluation unavailable (${failure}).`;
    status.textContent = `Evaluation unavailable (${failure}). Waiting for the next request.`;
    return;
  }
  const local = result.inference_source === "on_device";
  executionRoute.textContent = local ? "On this device" : "In the cloud";
  evaluationModel.textContent = result.model;
  evaluationClassification.textContent = result.observation.classification.replaceAll("_", " ");
  evaluationResult.textContent = result.observation.brief_description;
  status.textContent = local ? "Last evaluation ran on this device." : "Last evaluation ran in the cloud.";
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
