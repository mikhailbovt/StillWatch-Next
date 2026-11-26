import modelLock from "../provider-models.lock.json" with { type: "json" };

export const firebaseSdkVersion = modelLock.firebase_js_sdk;
export const maxImages = 5;
export const maxImageBytes = 384 * 1024;
export const maxRequestBytes = maxImages * maxImageBytes + 16 * 1024;

export function readConfiguration(value) {
  const { mode, model, firebase, appCheck } = value;
  if (!["cloud_node", "hybrid_web"].includes(mode) || !modelLock.gemini_developer_api.includes(model)) {
    throw new Error("Invalid Firebase worker mode or unlocked model");
  }
  for (const field of ["apiKey", "projectId", "appId"]) {
    if (typeof firebase?.[field] !== "string" || !/^[\w:.-]{1,150}$/.test(firebase[field])) {
      throw new Error("Invalid Firebase client configuration");
    }
  }
  if (!["token_file", "development_debug"].includes(appCheck)) {
    throw new Error("An App Check source is required");
  }
  return { mode, model, appCheck, firebase: { apiKey: firebase.apiKey, projectId: firebase.projectId, appId: firebase.appId } };
}
