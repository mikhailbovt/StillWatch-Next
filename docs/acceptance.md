# StillWatch Next acceptance evidence

Use the [release obligations](../qualification/release-requirements.json) to trace the original criteria and [Fast RC v3 Short](../qualification/README.md#fast-rc-v3-short) to reproduce qualification. The [implementation status](../IMPLEMENTATION_STATUS.md) records the measured results for 0.1.0. Earlier results apply to the builds and workloads they measured.

| Area | Evidence | Open checks |
| --- | --- | --- |
| Autonomy | `tests/e2e/installed_cpu.sh` exercises offline Core and the local models. Windows SCM and Linux systemd have been exercised through reboot and service recovery. | Isolated qualification of the exact release packages. |
| Rust migration | The model locks, frozen observations and `models/evaluations/` record the reference weights and parity checks. On Linux CUDA the active Balanced Pose and Fire roles passed. The inactive full Pose role exceeded a tensor tolerance on the gray fixture. | Independent scene and hardware equivalence. |
| Component separation | Core, capture and inference run in separate processes. Desktop and mobile packages carry the Companion runtime and native plugins. The Rust SDK excludes the engine, ORT and FFmpeg. | Distribution checks on each final signed artifact. |
| Cameras | `rtsp_core.py`, `rtsps_core.py`, `onvif_core.py` and `usb_core.py` cover TCP and UDP, TLS, ONVIF test-server profiles, reconnect and source epochs. Physical webcam capture was exercised on Windows and Linux UVC under WSL2. | Physical ONVIF and deployment-specific camera capacity. |
| Vision | `analysis_core.py` exercises native ONNX detection and evidence persistence. The [evaluation manifests](../qualification/README.md#evaluation-inputs-and-interpretation) preserve the datasets and scoring protocol. | Timely early fire and falls, realistic nuisance scenes, and independent identity and false-alert estimates. |
| Firebase | `tests/e2e/firebase_worker.py` exercises consent, routing, outages and freshness. Release App Check issuance, rotation and expiry were observed. A packaged Core applied a real Gemini result and submitted one opening check for the consented incident. | Production monitoring and deployment-specific credentials. |
| Safety semantics | The reducer and outbox tests cover scoped evidence, receipts, acknowledgement, resolution and repeat delivery. The local detection path currently opens Review. | Autonomous Urgent on representative footage. |
| Durable state | The accepted-opening journal survives epoch changes and retries. Storage tests cover rollback, bounded retention, crash cutpoints, evidence and outbox recovery. | Reproduce the applicable storage matrix for each qualified profile. |
| Access | `lan_core.py`, `proxy_core.py` and `gateway_core.py` test pairing, scope, revocation, TLS, canonical origin and relaying. | Production network and DNS configuration for each deployment. |
| Live video | `whep_core.py` exercises two masked H.264 viewers, session renewal, detail decoder sharing and local NAT/TURN. | Traversal between independent Internet networks. |
| SDK | Rust, Python, Dart and TypeScript have protocol test suites. The generated contracts can be checked with `cargo xtask contracts --check`. | Cross-version acceptance on the supported deployment pair. |
| Applications | The platform bundles and smoke tests are listed in the [implementation status](../IMPLEMENTATION_STATUS.md#companion). | Physical Android and iOS runtime acceptance. |
| Alerts | `alarm_agent.py` exercises receipts and restart deduplication. The owner confirmed audible Windows and Linux output. | Physical mobile delivery and production push credentials. |
| Optimization | The latest short measurement covered Windows CPU4 and Linux CUDA16 with viewers and decoded evidence. The CUDA device peak was 4.77 GiB on the shared host. | Full isolated load and recovery qualification. |
| Recovery | Native upgrade 17 → 18, Windows backup restore on Linux and rollback to 17 preserved settings, incident and evidence hashes. | Repeat backup and recovery under the production host's storage configuration. |
| Packaging | Core MSI and Linux CPU/CUDA packages were checked against their installed payload manifests. Android signatures and the iOS Apple Distribution IPA were verified. App Store Connect accepted the iOS build. | Windows signing and redistribution rights for the exact payloads. |
| Operations | The [operations guide](operations.md) covers setup, profiles, diagnostics, backup and distribution. [Remote access](remote.md) covers VPN, HTTPS and relay deployment. | Site-specific acceptance and support information before broader publication. |

## Distribution

- G1 covers the [Hybrid Web demonstration](hybrid-review.md), contest eligibility and public submission.
- G2 retains the [Firebase worker](firebase-worker.md) release configuration and App Check evidence.
- G3 follows the [physical device checks](../qualification/device-run.md) for push and deployment.
- G4 requires the release owner to confirm rights for the [distribution inventory](operations.md#distribution-inventory).
- G5 requires the applicable [native and quality protocols](../qualification/README.md) for the exact candidate. The owner selected SignPath Foundation for public Windows signing.
