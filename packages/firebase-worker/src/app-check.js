const MAX_TOKEN = 12 * 1024;
const DEBUG_TOKEN = /^[\w-]{16,128}$/;
const REFRESH_RESERVE_MS = 5 * 60_000;

export function appCheckProvider(config, credentialFile, readFile) {
  let cached;
  const { projectId, appId, apiKey } = config.firebase;
  const readToken = async () => validToken(
    (await readFile(credentialFile, MAX_TOKEN, true)).toString("ascii").trim(), config.firebase,
  );
  const readDebugToken = async () => {
    const token = (await readFile(credentialFile, 128, true)).toString("ascii").trim();
    if (!DEBUG_TOKEN.test(token)) throw new Error("invalid_app_check_credential");
    return token;
  };
  const acquire = async signal => {
    if (config.appCheck === "token_file") {
      return readToken();
    }
    if (cached && cached.expireTimeMillis > Date.now() + 60_000) return cached;
    const debugToken = await readDebugToken();
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
    cached = validToken(data.token, config.firebase);
    return cached;
  };
  const credentialStatus = async () => {
    try {
      if (config.appCheck === "token_file") {
        const { expireTimeMillis } = await readToken();
        return expireTimeMillis <= Date.now() + REFRESH_RESERVE_MS ? "renewal_due" : "ready";
      }
      await readDebugToken();
      return "development_debug";
    } catch {
      return "unavailable";
    }
  };
  return { acquire, credentialStatus };
}

export function validToken(token, { projectId, appId }) {
  if (typeof token !== "string" || token.length > MAX_TOKEN || !/^[\w-]+\.[\w-]+\.[\w-]+$/.test(token)) {
    throw new Error("app_check_unavailable");
  }
  const [header, payload] = token.split(".");
  const algorithm = decode(header)?.alg;
  const claims = decode(payload);
  const expiry = claims?.exp * 1_000;
  const projectNumber = appId.split(":")[1];
  // Firebase verifies the signature on the API request. These checks keep
  // unusable or misrouted credentials out of the worker's ready state.
  if (algorithm !== "RS256" || !Number.isSafeInteger(claims?.exp) || !Number.isSafeInteger(expiry)
      || expiry <= Date.now() + 15_000 || claims.sub !== appId
      || claims.iss !== `https://firebaseappcheck.googleapis.com/${projectNumber}`
      || !Array.isArray(claims.aud) || !claims.aud.includes(`projects/${projectId}`)) {
    throw new Error("app_check_unavailable");
  }
  return { token, expireTimeMillis: expiry };
}

function decode(part) {
  return JSON.parse(Buffer.from(part, "base64url").toString("utf8"));
}
