import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
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
  const stream = client.changes(new Cursor(snapshot.cursor), AbortSignal.timeout(30_000));
  try {
    const change = (await stream.next()).value;
    assert.equal(change?.change?.payload?.camera_id, options.camera);
  } finally {
    await stream.return();
  }
  process.stdout.write("typescript pairing, refresh and scoped SSE verified\n");
} finally {
  client.close();
}
