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

test("the warmup deadline no longer controls a successfully warmed session", async t => {
  const deadline = new AbortController();
  t.mock.method(AbortSignal, "timeout", () => deadline.signal);
  let creationSignal;
  let imageClosed = false;
  const session = { async prompt() {}, destroy: t.mock.fn() };
  globalThis.LanguageModel = { async create({ signal }) { creationSignal = signal; return session; } };
  globalThis.OffscreenCanvas = class {
    getContext() { return { fillRect() {} }; }
    transferToImageBitmap() { return { close() { imageClosed = true; } }; }
  };
  t.after(() => { delete globalThis.LanguageModel; delete globalThis.OffscreenCanvas; });

  assert.equal(await warmDevice(), session);
  assert.equal(imageClosed, true);
  deadline.abort(new DOMException("Warmup timed out", "TimeoutError"));
  assert.equal(creationSignal.aborted, false);
  assert.equal(session.destroy.mock.callCount(), 0);
});
