const MAX_TOKEN = 12 * 1024;

export function appCheckProvider(config, credentialFile, readFile) {
  let cached;
  return async signal => {
    if (config.appCheck === "token_file") {
      return tokenFromFile((await readFile(credentialFile, MAX_TOKEN, true)).toString("ascii").trim());
    }
    if (cached && cached.expireTimeMillis > Date.now() + 60_000) return cached;
    const debugToken = (await readFile(credentialFile, 128, true)).toString("ascii").trim();
    if (!/^[\w-]{16,128}$/.test(debugToken)) throw new Error("invalid_app_check_credential");
    const { projectId, appId, apiKey } = config.firebase;
    const url = new URL("https://firebaseappcheck.googleapis.com/v1/projects/" + encodeURIComponent(projectId)
      + "/apps/" + encodeURIComponent(appId) + ":exchangeDebugToken");
    const response = await fetch(url, {
      method: "POST", redirect: "error",
      headers: { "Content-Type": "application/json", "X-Goog-Api-Key": apiKey },
      body: JSON.stringify({ debugToken, limitedUse: false }),
      signal: AbortSignal.any([signal, AbortSignal.timeout(4_000)]),
    });
    if (!response.ok || !response.body) throw new Error("app_check_unavailable");
    const chunks = [];
    let length = 0;
    for await (const chunk of response.body) {
      length += chunk.byteLength;
      if (length > MAX_TOKEN) throw new Error("app_check_unavailable");
      chunks.push(chunk);
    }
    const data = JSON.parse(Buffer.concat(chunks).toString("utf8"));
    cached = tokenFromFile(data.token);
    return cached;
  };
}

function tokenFromFile(token) {
  if (typeof token !== "string" || token.length > MAX_TOKEN || !/^[\w-]+\.[\w-]+\.[\w-]+$/.test(token)) {
    throw new Error("app_check_unavailable");
  }
  const claims = JSON.parse(Buffer.from(token.split(".")[1], "base64url").toString("utf8"));
  const expiry = Number(claims.exp) * 1_000;
  if (!Number.isSafeInteger(expiry) || expiry <= Date.now() + 15_000) throw new Error("app_check_unavailable");
  return { token, expireTimeMillis: expiry };
}
