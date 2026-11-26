import assert from "node:assert/strict";
import { chmod, mkdtemp, readFile, rename, rmdir, unlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { appCheckProvider, validToken } from "./app-check.js";

const firebase = { projectId: "stillwatch-local-test", appId: "1:1234:web:12345678" };

// Firebase verifies the signature when handling the API request.
function token(expiresIn, overrides = {}, alg = "RS256") {
  const encode = value => Buffer.from(JSON.stringify(value)).toString("base64url");
  const payload = {
    exp: Math.floor(Date.now() / 1_000) + expiresIn,
    sub: firebase.appId,
    iss: "https://firebaseappcheck.googleapis.com/1234",
    aud: ["projects/1234", "projects/stillwatch-local-test"],
    ...overrides,
  };
  return `${encode({ alg })}.${encode(payload)}.local_fixture`;
}

test("the production provider picks up renewal and fails closed on expiry", async t => {
  const folder = await mkdtemp(path.join(tmpdir(), "stillwatch-app-check-"));
  const current = path.join(folder, "app-check.jwt");
  const next = path.join(folder, "next.jwt");
  t.after(async () => {
    for (const file of [current, next]) {
      await unlink(file).catch(error => { if (error.code !== "ENOENT") throw error; });
    }
    await rmdir(folder);
  });
  if (process.platform !== "win32") await chmod(folder, 0o700);
  const publish = async value => {
    await writeFile(next, `${value}\n`, { mode: 0o600, flag: "wx" });
    await rename(next, current);
  };

  const provider = appCheckProvider({ firebase, appCheck: "token_file" }, current,
    async (file, limit, requirePrivate) => {
      assert.equal(file, current);
      assert.equal(limit, 12 * 1024);
      assert.equal(requirePrivate, true);
      return readFile(file);
    });
  assert.equal(await provider.credentialStatus(), "unavailable");
  const expiring = token(120);
  await publish(expiring);
  assert.equal(await provider.credentialStatus(), "renewal_due");
  assert.equal((await provider.acquire()).token, expiring);
  const replacement = token(3_600);
  await publish(replacement);
  assert.equal(await provider.credentialStatus(), "ready");
  assert.equal((await provider.acquire()).token, replacement);
  await publish(token(-60));
  assert.equal(await provider.credentialStatus(), "unavailable");
  await assert.rejects(provider.acquire(), /app_check_unavailable/);
});

test("a production App Check token has to name the designated Firebase app and project", () => {
  assert.equal(validToken(token(1_800), firebase).token.split(".").length, 3);
  for (const invalid of [
    token(1_800, { sub: "1:1234:web:different" }),
    token(1_800, { iss: "https://firebaseappcheck.googleapis.com/1111" }),
    token(1_800, { aud: ["projects/different"] }),
    token(1_800, {}, "HS256"),
    token(0),
  ]) {
    assert.throws(() => validToken(invalid, firebase), /app_check_unavailable/);
  }
});
