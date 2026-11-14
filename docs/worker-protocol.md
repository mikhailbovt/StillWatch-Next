# Bundled adapters and worker IPC

StillWatch compiles bundled Rust adapters with the selected Core release. The source interfaces are `sw_capture::SourceAdapter`, `sw_inference::PerceptionBackend`, `sw_domain::HazardPolicy`, `sw_engine::SemanticProvider`, and `NotificationTransport` in the alarm agent. The capture adapter owns one authorized source. Tensor evaluation is synchronous inside an inference worker and returns a borrowed output to its decoder. The policies receive typed observations and scene state. The semantic trait receives a masked evidence packet. The notification trait receives alert metadata after the agent checks its current Core session. The supervisors own deadlines, access rights, persistence and acknowledgement.

The shipped Core comes with the reviewed worker binary. The base message schemas are under `packages/contracts/workers/v1` in source and `docs/worker-protocol/v1` in native packages. The contract carries sensitive source information to the capture worker over an inherited pipe on the Core host. Treat the capture and perception executables as trusted local code. An operating-system process restriction, if required by the deployment, belongs to the service administrator. Companion has no plugin installation interface.

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

Workers advertising `capture_source_metadata` support `capture-with-source-metadata`. The stream starts with the existing `SWC1` codec record followed by one little-endian u32 for the demuxer's advertised rate in milliframes per second. Zero denotes unknown. Published frames retain the same header, RGB format and sequence. Core observes successive native PTS values separately when it probes a source. Older workers can continue using the original modes.

## Inference

Core launches `stillwatch-worker infer` and sends a little-endian u32 byte count followed by `InferenceConfig.json` UTF-8. The payload limit is 16,384 bytes. The worker replies with four bytes. `SWI1` means both live models are ready. `SWIM`, `SWIS` and `SWIR` report pack, session and runtime failures respectively. Startup is bounded to 30 seconds on CPU and 60 seconds on CUDA.

When Core has a verified execution profile, `live_profile` pins the PoseBalanced and Fire graph hashes and at most 16 allowed tensor shapes. Before READY, the worker independently checks the role hashes, warms both sessions on every shape, and rejects subsequent frames outside that inventory. The pack validator still checks every graph. Offline `verify-model-pack` reports artifact admission, numerical parity for the full pack and active roles, and observation parity. `profile-model-pack` runs those checks and writes ONNX Runtime placement traces to a newly created directory.

Each request sends a 16-byte UUID in standard byte order, one task byte (`1` pose, `2` fire, `3` both), and the same `SWF1` header and RGB24 payload. The frame must have a native source timestamp and pass privacy masking in capture. Each reply has a little-endian u32 length and a UTF-8 `InferenceResult.json` body, up to 1,500,000 bytes. The reply echoes the UUID, reports inference time and includes a list or null for each task. An empty executed list is distinct from null. Results have at most 300 observations per task, use coordinates in source pixels and preserve the frame's source time. Core validates them again and terminates a worker that violates bounds or exceeds the two-second exchange deadline.

When `capture-doctor` reports `inference_timing_v2=true`, Core uses `infer-v2` with ready marker `SWI2`. Each response uses the base length and body followed by a 28-byte trailer. The trailer holds seven little-endian u32 durations in microseconds, in the order below. Unexecuted task stages carry zero. Core defaults to `infer` when the capability is absent. An advertised command with inconsistent framing is a worker failure.

When the worker also reports `fire_proposals=true`, Core uses `infer-v2-proposals` with the same request and timing trailer. The [reply schema](../packages/contracts/workers/v2-proposals/FireProposalResult.json) wraps the legacy inference result and includes up to eight geometry-checked, low-score Fire proposals. Proposals can schedule extra observation or maintain short-term support for a matched region. Review decisions still require the existing detector threshold and policy evidence.

| Offset | Stage |
| ---: | --- |
| 0 | `input_read_us`, after the worker reads the first request byte through completion of RGB read |
| 4 | `preprocess_us`, RGB validation and letterbox/tensor preparation |
| 8 | `pose_run_us`, Pose backend evaluation including input and output checks |
| 12 | `pose_decode_us`, Pose observations including appearance computation |
| 16 | `fire_run_us`, Fire backend evaluation including input and output checks |
| 20 | `fire_decode_us`, Fire observations including score filtering and NMS |
| 24 | `serialize_us`, encoding the base result JSON |

Core measures round-trip, frame receive to dispatch and frame receive to acceptance on its own monotonic clock. The worker measures only durations. Round-trip includes request delivery, worker stages, response delivery and Core validation. A timed session exposes a worker stage sample for each valid reply, including replies discarded by Core for freshness or source scope. A legacy session still measures Core timings.

The local `diagnose-fire` command accepts the same config and fire-only requests. Its ready marker is `SWD1`. The bounded JSON response includes the frame's source PTS, source dimensions in height/width order, the legacy observations and a `trace` object. The trace includes tensor geometry, resize and padding, a disposition count for each class and up to 16 anchors per class with their scores, boxes and rejection reasons. `tools/benchmark/fire_observations.py --diagnostic` records this output for short windows of authorized development clips. The tool processes one clip at a time with a maximum of 32 clips, 30 seconds per window and 2,048 samples per clip. Core has no HTTP route for this command.

A development Core built with `cargo build -p stillwatch-core --features diagnostics` adds a local `trace-fire-policy` command. Pass that executable to the benchmark with `--policy-core`. The harness feeds the identical masked RGB, native PTS and legacy detector observations to the default fire policy. It records scene visibility, continuity, region and fire decision. The diagnostic models an unreviewed scene and a fixed offline sampling sequence. The separate paced run supplies recorded incident openings.

## Semantic provider

The bundled provider uses authenticated HTTP on loopback. `GET /health` negotiates the locked Firebase SDK and model, route capabilities, busy state and the packet limits. `POST /evaluate` carries a little-endian u32 metadata length, a UTF-8 evidence packet with a byte index and deadline, and its concatenated masked JPEGs. See `firebase-worker.md` for the runtime and credential setup. The Core validation and Firebase-only deployment policy apply after the transport returns. Run `cargo xtask contracts --check` to verify the generated worker schemas alongside the public OpenAPI schemas.
