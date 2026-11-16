import assert from "node:assert/strict";
import { test } from "node:test";
import { deleteApp, initializeApp } from "firebase/app";
import { evaluate, makeAI, warmDevice } from "./generate.js";

function fixture(t, route) {
  const app = initializeApp({ projectId: "stillwatch-local-test", apiKey: "local-test-key",
    appId: "1:123:web:local-test" }, { name: t.name, automaticDataCollectionEnabled: false });
  t.after(() => deleteApp(app));
  const images = [
    { id: "f1", kind: "overview", source_s: 1, jpeg: new Uint8Array([1, 2]) },
    { id: "f2", kind: "overview", source_s: 2, jpeg: new Uint8Array([3, 4]) },
    { id: "f3", kind: "crop", source_s: 2, jpeg: new Uint8Array([5, 6]) },
  ];
  return { ai: makeAI(app), packet: { meta: { route, category: "fire", context: {}, timeout_ms: 2_000 }, images } };
}

test("local-only evidence stays off the cloud when the browser adapter is absent", async t => {
  const { ai, packet } = fixture(t, "local_only");
  const network = t.mock.method(globalThis, "fetch", async () => {
    throw new Error("unexpected cloud dispatch");
  });
  await assert.rejects(evaluate(ai, "gemini-2.5-flash", packet, AbortSignal.timeout(2_000),
    { browser: true, deviceReady: true }), { message: "route_unavailable" });
  assert.equal(network.mock.callCount(), 0);
});

test("cloud fallback includes the temporal evidence when the device model is unavailable", async t => {
  const { ai, packet } = fixture(t, "prefer_local");
  const received = [];
  const network = t.mock.method(globalThis, "fetch", async (_, init) => {
    const request = JSON.parse(init.body);
    received.push(...request.contents[0].parts.flatMap(part => part.inlineData ? [part.inlineData.data] : []));
    const observation = { classification: "insufficient_evidence",
      observations: { flame_visible: "unknown", smoke_visible: "unknown", apparent_spread: "unknown",
        contained_in_designated_region: "unknown", controlled_source_supported: "unknown", visibility: "limited" },
      supporting_image_ids: ["f1", "f3"], contradictions: [], limitations: [], brief_description: "Limited view." };
    return Response.json({ candidates: [{ finishReason: "STOP", content: {
      parts: [{ text: JSON.stringify(observation) }],
    } }] });
  });
  const result = await evaluate(ai, "gemini-2.5-flash", packet, AbortSignal.timeout(2_000),
    { browser: true, deviceReady: false });
  assert.equal(result.inference_source, "in_cloud");
  assert.equal(network.mock.callCount(), 1);
  assert.deepEqual(received, packet.images.map(image => Buffer.from(image.jpeg).toString("base64")));
});

test("creation and image warmup have independent deadlines", async t => {
  const creationDeadline = new AbortController();
  const imageDeadline = new AbortController();
  const deadlines = new Map([[120_000, creationDeadline], [60_000, imageDeadline]]);
  t.mock.method(AbortSignal, "timeout", ms => deadlines.get(ms).signal);
  let creationSignal;
  let imageClosed = false;
  const session = {
    async prompt(_, { signal }) {
      creationDeadline.abort(new DOMException("Creation timed out", "TimeoutError"));
      assert.equal(creationSignal.aborted, false);
      assert.equal(signal, imageDeadline.signal);
    },
    destroy: t.mock.fn(),
  };
  globalThis.LanguageModel = { async create({ signal }) { creationSignal = signal; return session; } };
  globalThis.OffscreenCanvas = class {
    getContext() { return { fillRect() {} }; }
    transferToImageBitmap() { return { close() { imageClosed = true; } }; }
  };
  t.after(() => { delete globalThis.LanguageModel; delete globalThis.OffscreenCanvas; });

  assert.equal(await warmDevice(), session);
  assert.equal(imageClosed, true);
  imageDeadline.abort(new DOMException("Image warmup timed out", "TimeoutError"));
  assert.equal(creationSignal.aborted, false);
  assert.equal(session.destroy.mock.callCount(), 0);
});

test("warmup reports which deadline expired and releases a failed session", async t => {
  let creationDeadline = new AbortController();
  let creationPending = true;
  const imageDeadline = new AbortController();
  const timeout = new DOMException("Warmup timed out", "TimeoutError");
  t.mock.method(AbortSignal, "timeout", ms => ms === 120_000 ? creationDeadline.signal : imageDeadline.signal);
  const closeImage = t.mock.fn();
  const session = {
    async prompt(_, { signal }) {
      imageDeadline.abort(timeout);
      signal.throwIfAborted();
    },
    destroy: t.mock.fn(),
  };
  globalThis.LanguageModel = { async create({ signal }) {
    if (creationPending) {
      await new Promise((_, reject) => signal.addEventListener("abort", () => reject(signal.reason), { once: true }));
    }
    return session;
  } };
  globalThis.OffscreenCanvas = class {
    getContext() { return { fillRect() {} }; }
    transferToImageBitmap() { return { close: closeImage }; }
  };
  t.after(() => { delete globalThis.LanguageModel; delete globalThis.OffscreenCanvas; });

  const first = warmDevice();
  creationDeadline.abort(timeout);
  await assert.rejects(first, { message: "device_session_deadline" });
  creationPending = false;
  creationDeadline = new AbortController();
  await assert.rejects(warmDevice(), { message: "device_image_deadline" });
  assert.equal(closeImage.mock.callCount(), 1);
  assert.equal(session.destroy.mock.callCount(), 1);
});
