<img src="apps/companion/assets/mark.png" width="64" height="64" alt="StillWatch mark">

# StillWatch Next

StillWatch monitors cameras from a Windows or Linux host and brings live video and incidents to a paired desktop or mobile Companion.

The Rust Core runs local ONNX models for fire, smoke and suspected falls. It saves incidents and masked evidence in SQLite, even when every Companion is closed. With camera consent and a ready Firebase AI Logic worker, Core checks the opening evidence once before publishing a Fire Review. Local monitoring continues when Firebase is unavailable.

| Component | What it does |
| --- | --- |
| Core | RTSP, ONVIF discovery, USB cameras and replay. Local detection, evidence, incident history and WebRTC through MediaMTX. |
| Companion | Windows, Linux, Android and iOS. Pair through pinned TLS, view cameras, review evidence and acknowledge or resolve incidents with the appropriate permissions. |
| Hybrid Web worker | Firebase AI Logic with on-device Gemini Nano in Chrome and consented cloud fallback. The worker shows which route ran. |
| SDK | Versioned Core API with Rust, Python, TypeScript and Dart clients. |

## Get started

- [Download StillWatch Core for Windows or Linux](https://github.com/mikhailbovt/StillWatch-Next/releases/tag/v1.0.1)
- [Download StillWatch Companion for Windows, Linux or Android](https://github.com/mikhailbovt/StillWatch-Next/releases/tag/v1.0.0)
- [Join the iPhone beta on TestFlight](https://testflight.apple.com/join/EM8yb4Ex)
- [Configure the Core and provision a model pack](docs/operations.md)
- [Build or pair a Companion](apps/companion/README.md)
- [Run the Hybrid Web walkthrough](docs/hybrid-review.md)
- [Watch the demo and connect Companion to a camera](docs/demo-access.md)
- [Configure LAN, VPN, HTTPS or an outbound relay](docs/remote.md)

The Core can start before a model pack is configured. Follow the [host prerequisites](tools/dev) and use the pinned Rust toolchain. Run `cargo xtask contracts --check` to check the protocol, schemas and generated API contracts.

## Release status

Core 1.0.1 works with Companion 1.0.0, including the iOS TestFlight build. It gives Gemini up to 15 seconds to check a new incident and can keep a brief smoke detection at Info when the images show a clear or controlled scene. Invitees need access to a Core to view cameras and incidents. Detection can be late, miss an event or produce a false alarm. See the [Core release notes and checksums](https://github.com/mikhailbovt/StillWatch-Next/releases/tag/v1.0.1) and [deployment limitations](LIMITATIONS.md).

StillWatch Next is maintained by Mikhail Bovt. [Support development on GitHub Sponsors](https://github.com/sponsors/mikhailbovt).

StillWatch Next source is available under [Apache-2.0](LICENSE). Models have their own licenses and SHA-256 locks. The repository distributes the model interfaces, pins and export tools. See the [model notices](models/THIRD_PARTY_NOTICES.md) and [native dependency inventory](docs/operations.md#distribution-inventory) before distributing packages.
