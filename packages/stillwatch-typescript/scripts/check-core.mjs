import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { Cursor, counter } from "../dist/index.js";
import { NodeStillWatchClient, enroll } from "../dist/node.js";

const [fixtureFile, credentialsFile] = process.argv.slice(2);
const fixture = JSON.parse(await readFile(fixtureFile, "utf8"));
for (const candidate of fixture.counter_valid) assert.equal(counter(candidate), candidate);
for (const candidate of fixture.counter_invalid) assert.throws(() => counter(candidate));

const options = JSON.parse(await readFile(credentialsFile, "utf8"));
const registration = await enroll(options.offer, "SDK QA TypeScript");
registration.credentials.access_expires_at = "2001-01-01T00:00:00Z";
let refreshes = 0;
const client = new NodeStillWatchClient(registration.core, {
  seed: registration.seed, credentials: registration.credentials,
  persist: async () => { refreshes++; },
});
try {
  assert.equal((await client.core()).instance_id, registration.core.core_instance_id);
  const [snapshot, , cameras] = await Promise.all([client.snapshot(), client.snapshot(), client.cameras()]);
  assert.equal(refreshes, 1);
  assert.deepEqual(cameras.cameras.map(camera => camera.id), [options.camera]);
  const markers = options.reconnect_markers;
  if (markers) await writeFile(join(markers, "ready-typescript"), "", { flag: "wx" });
  const stream = client.changes(new Cursor(snapshot.cursor), AbortSignal.timeout(60_000));
  try {
    const change = (await stream.next()).value;
    assert.equal(change?.change?.payload?.camera_id, options.camera);
    if (markers) {
      assert.equal(change.change.payload.detection_reason, "fixture 0");
      await writeFile(join(markers, "after-first-typescript"), "", { flag: "wx" });
      const next = (await stream.next()).value;
      assert.equal(next?.change?.payload?.camera_id, options.camera);
      assert.equal(next?.change?.payload?.detection_reason, "fixture 1000");
      assert.equal(next.cursor.generation, change.cursor.generation);
      assert(BigInt(next.cursor.sequence) > BigInt(change.cursor.sequence));
      await stream.return();

      const expired = client.changes(new Cursor(`${randomUUID()}:0`), AbortSignal.timeout(5000));
      try {
        await assert.rejects(expired.next(), { status: 409, code: "cursor_expired" });
      } finally {
        await expired.return();
      }
      const fresh = await client.snapshot();
      assert.equal(fresh.cursor, next.cursor.toString());
      assert.deepEqual(new Set(fresh.incidents.map(item => item.id)),
        new Set([change.change.payload.id, next.change.payload.id]));
    }
  } finally {
    await stream.return();
  }
  process.stdout.write("typescript pairing, refresh and scoped SSE verified\n");
} finally {
  client.close();
}
