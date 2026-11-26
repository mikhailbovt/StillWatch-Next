import { randomUUID } from "node:crypto";
import { lstat, open, rename, unlink } from "node:fs/promises";
import path from "node:path";
import { applicationDefault, deleteApp, initializeApp } from "firebase-admin/app";
import { getAppCheck } from "firebase-admin/app-check";
import { validToken } from "./app-check.js";
import { readConfiguration } from "./config.js";

async function publish(file, token) {
  const folder = path.dirname(file);
  const parent = await lstat(folder);
  if (!parent.isDirectory() || (process.platform !== "win32" && (parent.mode & 0o077) !== 0)) {
    throw new Error("App Check directory must be private");
  }
  const tmp = path.join(folder, `.app-check-${randomUUID()}`);
  const handle = await open(tmp, "wx", 0o600);
  try {
    try {
      await handle.writeFile(`${token}\n`, "ascii");
      await handle.sync();
    } finally {
      await handle.close();
    }
    await rename(tmp, file);
    if (process.platform !== "win32") {
      const directory = await open(folder, "r");
      try { await directory.sync(); } finally { await directory.close(); }
    }
  } finally {
    await unlink(tmp).catch(error => { if (error.code !== "ENOENT") throw error; });
  }
}

async function issue(configFile, tokenFile, serviceAccountId) {
  if (!configFile || !tokenFile || !path.isAbsolute(tokenFile)
      || (serviceAccountId && !/^[\w.-]+@[\w.-]+\.iam\.gserviceaccount\.com$/.test(serviceAccountId))) {
    throw new Error("Invalid production App Check issuance configuration");
  }
  const metadata = await lstat(configFile);
  if (!metadata.isFile() || metadata.size > 8192 || (process.platform !== "win32" && (metadata.mode & 0o077) !== 0)) {
    throw new Error("App Check configuration must be private");
  }
  const handle = await open(configFile, "r");
  let config;
  try {
    const bytes = Buffer.alloc(8193);
    const { bytesRead } = await handle.read(bytes, 0, bytes.length, 0);
    if (bytesRead !== metadata.size) throw new Error("App Check configuration changed while reading");
    config = readConfiguration(JSON.parse(bytes.toString("utf8", 0, bytesRead)));
  } finally {
    await handle.close();
  }
  if (config.appCheck !== "token_file") throw new Error("Production issuance requires token_file mode");
  const app = initializeApp({
    credential: applicationDefault(),
    projectId: config.firebase.projectId,
    serviceAccountId: serviceAccountId || undefined,
  });
  try {
    const result = await getAppCheck(app).createToken(config.firebase.appId, { ttlMillis: 60 * 60_000 });
    const { expireTimeMillis } = validToken(result.token, config.firebase);
    if (expireTimeMillis < Date.now() + 10 * 60_000) throw new Error("App Check token expiration is too soon");
    await publish(tokenFile, result.token);
    process.stdout.write(`App Check token published. Expires ${new Date(expireTimeMillis).toISOString()}.\n`);
  } finally {
    await deleteApp(app);
  }
}

const deadline = setTimeout(() => {
  process.stderr.write("App Check issuance timed out.\n");
  process.exit(1);
}, 30_000);
try {
  await issue(...process.argv.slice(2));
} catch (_) {
  process.stderr.write("App Check issuance failed. Check the production app, host identity, IAM signing rights and private token directory.\n");
  process.exitCode = 1;
} finally {
  clearTimeout(deadline);
}
