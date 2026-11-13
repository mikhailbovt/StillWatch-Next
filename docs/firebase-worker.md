# Firebase semantic worker

The Rust service owns incident decisions, evidence selection, deadlines, and the daily request budget. The worker runs alongside it on the same host and uses the pinned Firebase JS SDK 13.3.0. The hybrid profile uses a dedicated Chrome session with the Firebase on-device SDK. Its web page is a model bootstrap and health surface.

## Build and configure

Use Node 24.12 or newer in the 24.x line. Run these commands in the worker directory.

~~~text
npm ci
npm run build
~~~

Create a private JSON configuration for a registered Firebase Web App. Enable Firebase AI Logic with the Gemini Developer API and configure App Check on that app. Copy the Firebase client configuration from the project settings. The current model allowlist contains gemini-3.5-flash-lite.

~~~json
{
  "mode": "cloud_node",
  "model": "gemini-3.5-flash-lite",
  "appCheck": "token_file",
  "firebase": {
    "apiKey": "FIREBASE_WEB_API_KEY",
    "projectId": "FIREBASE_PROJECT_ID",
    "appId": "FIREBASE_WEB_APP_ID"
  }
}
~~~

Supply an expiring Firebase App Check JWT through a private file and refresh it through the host's authorized issuance process before expiration. For a test project with a registered App Check debug secret, set appCheck to development_debug and supply that secret in the credential file. The worker exchanges it over HTTPS with Firebase App Check. Protect both files with owner-only permissions on Unix or a private directory ACL on Windows.

Generate a random 32-byte credential encoded as 64 lowercase hex characters for the Core-to-worker connection. Hybrid Web needs a second independent credential for the dedicated browser. Keep credential files out of source control and log collection.

~~~text
node src/server.js PRIVATE_CONFIG.json 8775 CORE_WORKER_TOKEN APP_CHECK_CREDENTIAL
stillwatch-core --data-dir DATA_DIR --semantic-worker 127.0.0.1:8775 --semantic-token-file CORE_WORKER_TOKEN --semantic-route cloud-only
~~~

For hybrid mode set mode to hybrid_web and append the browser credential file as the last argument to server.js. Open the loopback worker address in supported Chrome. Connect with the browser credential and use **Initialize Chrome model** to grant the user activation required by the SDK. Set Core's route to local-only, cloud-only, or prefer-local. The worker limits local evaluation to one current overview image. Prefer-local allows Firebase to use cloud inference when the device model is unavailable.

On a dedicated OS account and Chrome profile, enable **Reconnect in this dedicated browser profile** when connecting. Chrome stores only the separate browser credential at the loopback origin. Opening the same address with that profile reconnects the worker. After a model download the page checks whether Chrome can create a session without another download. **Disconnect browser** clears the saved credential. Protect the browser profile with the OS account permissions. On a shared browser, leave this setting off.

Enable **Allow Firebase cloud analysis** in the camera editor or set cloud_consent in its configuration to authorize sending masked evidence to Firebase AI Logic. Consent defaults to off, including for migrated cameras. With consent off, prefer-local can evaluate only on the device and only when the local model is ready. Cloud-only skips that camera. A consent change invalidates evidence from the previous capture epoch. A request already submitted to a provider remains subject to that provider's processing, and its result is discarded after revocation.

## Verify

GET /api/v2/semantic-status uses the normal Core authorization. The status reports the SDK, request budget, and the inference source from the last completed SDK response. A healthy loopback handshake establishes transport readiness. Use a licensed replay with --semantic-include-replay to qualify an actual end-to-end call, inspect the attempt in SQLite, and confirm the reported route. Replay is excluded from semantic evaluation by default.

The transport smoke test exercises both worker modes, rejected origin and credential, and the Rust health handshake without calling Firebase. Cloud App Check issuance, successful cloud and Chrome model execution, and Chrome session lifecycle remain separate deployment checks. See the [Firebase AI Logic App Check instructions](https://firebase.google.com/docs/ai-logic/app-check) and [Firebase Hybrid Web setup](https://firebase.google.com/docs/ai-logic/hybrid/web/get-started).
