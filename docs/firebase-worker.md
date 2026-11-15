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

Supply an expiring Firebase App Check JWT through a private file. Use the host's authorized issuance process to replace it atomically before expiration. The worker checks expiry, issuer, app ID and the project audience locally. Firebase verifies the token when it processes the AI Logic request. For a test project with a registered App Check debug secret, set appCheck to development_debug and supply that secret in the credential file. The worker exchanges it over HTTPS with Firebase App Check. Protect both files with owner-only permissions on Unix or a private directory ACL on Windows. [Firebase App Check token claims](https://firebase.google.com/docs/reference/admin/node/firebase-admin.app-check.decodedappchecktoken)

On a managed Core host use a dedicated Firebase app and an authorized Application Default Credentials identity. The separate issuer uses the Admin SDK to sign for that app. Grant the workload identity the minimum permission required to sign with the designated service account. Keep the issuer's ADC outside the worker's environment. [Custom App Check provider](https://firebase.google.com/docs/app-check/custom-provider), [Application Default Credentials](https://cloud.google.com/docs/authentication/application-default-credentials)

Run `node src/issue-app-check.js PRIVATE_CONFIG /absolute/private/app-check.jwt SIGNING_SERVICE_ACCOUNT`. The last argument is optional when ADC can choose the signing account. The issuer accepts `token_file` configuration, publishes a one-hour JWT by atomic replacement and reports its expiry without printing its value. On Linux the supplied systemd timer runs it every ten minutes. Place the JWT in `/var/lib/stillwatch-firebase/` and set `CONFIG_FILE`, `APP_CHECK_FILE` and optionally `SIGNING_SERVICE_ACCOUNT` in the private issuer environment file. Put `CONFIG_FILE`, `PORT`, `CORE_CREDENTIAL_FILE`, `APP_CHECK_FILE`, and `BROWSER_CREDENTIAL_FILE` for hybrid mode in the worker environment file. Use units from `deploy/systemd/`. On Windows run the issuer as a scheduled task and the worker as a service under the dedicated account. Set `STILLWATCH_FIREBASE_PRODUCTION=1` in the production worker environment to require `token_file`. The supplied Linux worker unit sets it. Check the timer or task result and Core's `app_check_state`. `renewal_due` indicates that less than five minutes of validity remain. A failed renewal keeps the previous JWT until its expiration. Core's local ONNX analysis can continue during an App Check outage.

Generate a random 32-byte credential encoded as 64 lowercase hex characters for the Core-to-worker connection. Hybrid Web needs a second independent credential for the dedicated browser. Keep credential files out of source control and log collection.

~~~text
node src/server.js PRIVATE_CONFIG.json 8775 CORE_WORKER_TOKEN APP_CHECK_CREDENTIAL
stillwatch-core --data-dir DATA_DIR --semantic-worker 127.0.0.1:8775 --semantic-token-file CORE_WORKER_TOKEN --semantic-route cloud-only
~~~

For hybrid mode set mode to hybrid_web and append the browser credential file as the last argument to server.js. Open the loopback worker address in supported Chrome. Connect with the browser credential and use **Initialize Chrome model** to grant the user activation required by the SDK. Set Core's route to local-only, cloud-only, or prefer-local. The worker requires a ready Chrome adapter for local-only and uses one current overview for local inference. With cloud consent and a model already known to be unavailable, prefer-local sends the bounded temporal packet to the cloud route.

On a dedicated OS account and Chrome profile, enable **Reconnect in this dedicated browser profile** when connecting. Chrome stores only the separate browser credential at the loopback origin. Opening the same address with that profile reconnects the worker. After a model download the page checks whether Chrome can create a session without another download. **Disconnect browser** clears the saved credential. Protect the browser profile with the OS account permissions. On a shared browser, leave this setting off.

The worker page shows the incident ID and evidence version of the latest evaluation, the requested route and the route reported by Firebase AI Logic. It displays the provider's classification and description. The masked overview is visible until the request deadline while the page stays visible. Hiding, freezing or leaving the page removes the image.

The browser maintains one authenticated request waiting for an evaluation. The worker completes it immediately when there is eligible evidence, or returns an idle response after four seconds. The browser then reopens the request. After startup, Chrome can take several polls to discover an installed model. The worker allows two minutes to open a local session and then one minute to warm it with a generated black frame. The dedicated tab retains that session while ready and releases it when leaving the page. The Core enforces the packet deadline for each evaluation. Keep the browser session running and verify model readiness after the host locks or restarts. [Chrome Prompt API](https://developer.chrome.com/docs/ai/prompt-api) · [Chrome background timer behavior](https://developer.chrome.com/blog/timer-throttling-in-chrome-88)

Enable **Allow Firebase cloud analysis** in the camera editor or set cloud_consent in its configuration to authorize sending masked evidence to Firebase AI Logic. Consent defaults to off, including for migrated cameras. With consent off, prefer-local can evaluate only on the device and only when the local model is ready. Cloud-only skips that camera. A consent change invalidates evidence from the previous capture epoch. A request already submitted to a provider remains subject to that provider's processing, and its result is discarded after revocation.

## Verify

GET /api/v2/semantic-status uses the normal Core authorization. The status reports the SDK, request budget, and the inference source from the last completed SDK response. A healthy loopback handshake establishes transport readiness. Use a licensed replay with --semantic-include-replay to qualify an actual end-to-end call, inspect the attempt in SQLite, and confirm the reported route. Replay is excluded from semantic evaluation by default.

For a repeatable Windows replay, start the hybrid worker, initialize the Chrome model and enable reconnect in the dedicated profile. Close and reopen Chrome and the Node worker on the same loopback address. Wait for the page to report model readiness. From the repository root, use the fixture runner below with the locked model pack, matching runtime and credited demo from `baseline.json`. The private configuration directory supplies the Core worker credential in `core-worker`.

~~~text
python -B tests/e2e/firebase_worker.py --node NODE --live-config PRIVATE_DIR --running-port 8775 --route local_only --expect-source on_device --core CORE --worker WORKER --demo CREDITED_DEMO --ffmpeg FFMPEG --models MODEL_PACK --runtime ORT_LIBRARY --manifest-sha256 MANIFEST_HASH --runtime-sha256 ORT_HASH
~~~

The replay requires the saved `on_device` result with cloud consent disabled. Repeat the command with `--route prefer_local --expect-source in_cloud` in a connected Chrome profile where the local model is unavailable. That case requires a cloud result for the opted-in camera and zero submissions for the camera with cloud consent off. Both commands check that replay creates no live alert.

The transport smoke test exercises both worker modes, rejected origin and credential, idle browser polling, delivery to a waiting browser, rejection of invalid result IDs, local-only attestation denial, and the Rust health handshake. Run cloud App Check issuance, actual inference and Chrome session lifecycle checks for the intended deployment. [Firebase AI Logic App Check instructions](https://firebase.google.com/docs/ai-logic/app-check), [Firebase Hybrid Web setup](https://firebase.google.com/docs/ai-logic/hybrid/web/get-started)

## Core dispatch and recovery

Core stores one semantic job and request ID for each fresh eligible evidence packet. A short fenced lease reserves the job while Core reads and checks the packet. Only the durable dispatch transition consumes the daily, incident and per-camera request budgets. Core sends the request ID as `X-StillWatch-Request-Id`. The worker forwards it to the browser job and echoes it to Core.

A dispatched job with an unknown response stays in the ledger. Core makes one transport attempt per packet because the provider can already have processed or billed an ambiguous request. A fresh packet may create another job within the configured budgets. Core saves validated responses in SQLite before applying them to the incident. It can retry saving or applying the response without submitting the packet again. Consent, camera scope, packet version and freshness are checked before dispatch and application. After a Core restart, pending evidence from the previous monotonic clock stays in history. A previously saved provider response remains available in the ledger.
