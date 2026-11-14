# StillWatch Next

StillWatch monitors cameras on a Windows or Linux host. The Rust Core decodes streams with libav, runs the locked ONNX models locally, and stores incidents and masked evidence in SQLite. It continues monitoring without Firebase or an open Companion.

The Windows, Linux, Android and iOS Companions pair with a Core to show video, review incidents and manage access. The optional Firebase AI Logic worker adds semantic evidence evaluation. Live video uses WebRTC through MediaMTX. The desktop AlarmAgent can run outside the Companion.

- [Install Core, add cameras and pair a Companion](docs/operations.md)
- [Configure the Firebase worker](docs/firebase-worker.md)
- [Connect over VPN, HTTPS or an outbound relay](docs/remote.md)
- [Build the Companion](apps/companion/README.md)
- [Inspect verification results and outstanding release checks](IMPLEMENTATION_STATUS.md)
- [Trace the acceptance criteria to tests and artifacts](docs/acceptance.md)
- [Read the completion plan and Fast RC v2 qualification contract](qualification/README.md)

The source, model and native dependency locks are in `Cargo.lock`, `models/models.rect.lock.json` and `models/native-deps.lock.json`. Use the host prerequisites in `tools/dev` to build the Core with the pinned Rust toolchain. Use `cargo xtask contracts --check` to verify the Rust protocol, JSON Schemas and generated API contracts.

The Rust SDK is in `crates/stillwatch-sdk`. The Python, Dart and TypeScript clients are in `packages`. The CPU profile is the reference for local operation. The current results cover desktop and server test hosts, Android emulation and an iOS Simulator. The acceptance matrix records the signing, physical equipment, external network and detection-quality checks still open.

Release qualification follows Fast RC v2, with a 180-minute budget per prepared native profile and a full hour of isolated continuous load. The earlier 24-hour and 72-hour runs are historical measurements for their pinned builds.
