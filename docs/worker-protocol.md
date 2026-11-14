# Bundled adapters and worker IPC

StillWatch compiles bundled Rust adapters with the selected Core release. The source interfaces are `sw_capture::SourceAdapter`, `sw_inference::PerceptionBackend`, `sw_domain::HazardPolicy`, `sw_engine::SemanticProvider`, and `NotificationTransport` in the alarm agent. The capture adapter owns one authorized source. Tensor evaluation is synchronous inside an inference worker and returns a borrowed output to its decoder. The policies receive typed observations and scene state. The semantic trait receives a masked evidence packet. The notification trait receives alert metadata after the agent checks its current Core session. The supervisors own deadlines, access rights, persistence and acknowledgement.

Use the v1 process wire contract to implement a compatible worker. The shipped Core comes with the reviewed worker binary. Schemas are under `packages/contracts/workers/v1` in source and `docs/worker-protocol/v1` in native packages. The contract carries sensitive source information to the capture worker over an inherited pipe on the Core host. Treat the capture and perception executables as trusted local code. An operating-system process restriction, if required by the deployment, belongs to the service administrator. Companion has no plugin installation interface.

Core starts its workers, MediaMTX and media encoder in Windows job objects or Linux process groups. On Linux the direct child also exits when its Core parent dies, including when the foreground Core is killed. Keep the packaged service supervisor in control of the whole process tree. A Linux wrapper for a worker should use `exec` to replace itself with the worker. [Linux parent-death signals](https://man7.org/linux/man-pages/man2/PR_SET_PDEATHSIG.2const.html)

## Capture

Core launches `stillwatch-worker capture`, sends one UTF-8 `CaptureRequest` from `packages/contracts/workers/v1/CaptureRequest.json`, and closes stdin. The limit is 16,384 bytes. Stdout is binary. An orderly exit closes stdout. Exit code 4 signals that the configured source could not be opened or decoded. Keep diagnostics on stderr and redact credentials. Core runs `capture-doctor` and requires `rtsp_peer_confined=true` before enabling the network capture adapter. This flag describes a decoder that rejects redirects and confines negotiated media to the authorized peer.

When `capture-doctor` reports `capture_codec=true`, Core launches `capture-with-codec` with the same request. The worker sends one 36-byte header immediately before its first decoded frame. The first four bytes are ASCII `SWC1`. A codec name of 1 to 32 ASCII lowercase letters, digits or underscores follows with zero padding. The original `capture` command starts directly with its first frame.

Each frame starts with a 56-byte header, followed immediately by interleaved RGB24 rows without padding. Numeric fields are little-endian.

| Offset | Width in bytes | Value |
| ---: | ---: | --- |
| 0 | 4 | ASCII `SWF1` |
| 4 | 1 | Clock quality. `0` missing, `1` native, `2` estimated |
| 5 | 1 | Corruption flag. `0` or `1` |
| 6 | 2 | Reserved zero |
| 8 | 4 | Width, unsigned, 1–3840 |
| 12 | 4 | Height, unsigned, 1–2160 |
| 16 | 4 | Payload length, unsigned, exactly width × height × 3 |
| 20 | 4 | Reserved zero |
| 24 | 8 | Unsigned frame sequence, starting at 1 |
| 32 | 8 | Signed raw presentation timestamp |
| 40 | 8 | Signed source presentation time in microseconds |
| 48 | 4 | Signed time-base numerator |
| 52 | 4 | Signed time-base denominator |

Use the minimum signed 64-bit value for absent timestamps. For a native clock both timestamps are present, the time base is positive, and conversion uses integer division toward zero. For an estimated clock the source time is present and the raw timestamp is absent. Missing quality has no usable microsecond timestamp. Core bounds raw-frame memory across sources, drops a frame when its permit budget is exhausted and supervises the capture process deadline. The first frame may have a longer deadline on Windows RTSPS while Schannel validates its certificate.

## Inference

Core launches `stillwatch-worker infer` and sends a little-endian u32 byte count followed by `InferenceConfig.json` UTF-8. The payload limit is 16,384 bytes. The worker replies with four bytes. `SWI1` means both live models are ready. `SWIM`, `SWIS` and `SWIR` report pack, session and runtime failures respectively. Startup is bounded to 30 seconds on CPU and 60 seconds on CUDA.

Each request sends a 16-byte UUID in standard byte order, one task byte (`1` pose, `2` fire, `3` both), and the same `SWF1` header and RGB24 payload. The frame must have a native source timestamp and pass privacy masking in capture. Each reply has a little-endian u32 length and a UTF-8 `InferenceResult.json` body, up to 1,500,000 bytes. The reply echoes the UUID, reports inference time and includes a list or null for each task. An empty executed list is distinct from null. Results have at most 300 observations per task, use coordinates in source pixels and preserve the frame's source time. Core validates them again and terminates a worker that violates bounds or exceeds the two-second exchange deadline.

## Semantic provider

The bundled provider uses authenticated HTTP on loopback. `GET /health` negotiates the locked Firebase SDK and model, route capabilities, busy state and the packet limits. `POST /evaluate` carries a little-endian u32 metadata length, a UTF-8 evidence packet with a byte index and deadline, and its concatenated masked JPEGs. See `firebase-worker.md` for the runtime and credential setup. The Core validation and Firebase-only deployment policy apply after the transport returns. Run `cargo xtask contracts --check` to verify the generated worker schemas alongside the public OpenAPI schemas.
