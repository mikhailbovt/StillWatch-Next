# Native boundaries and CPU model qualification

## Accepted runtime design

Core owns camera configuration, scheduling, incidents and delivery intent. Capture and inference run in supervised native worker processes. The worker uses libav for video and ONNX Runtime for inference. Its only Python environment is the developer's separate exporter and reference oracle.

The selected compiler is Rust 1.97.1, edition 2024. The baseline profile uses four ORT CPU threads, sequential execution, graph optimization, and a static RGB FP32 `1×3×640×640` input. The Rust model loader verifies the library and the pack against trusted digests, then validates ONNX session I/O. The application supplies those digests from a trusted installation or an explicitly selected development pack. `models/native-deps.lock.json` identifies the exact tested binaries and Ubuntu packages.

| Model | Checked ONNX layout | Baseline postprocessing |
| --- | --- | --- |
| YOLO26n pose | `[1, 56, 8400]` | XYWH and COCO17 keypoints, confidence 0.15, class-aware NMS at IoU 0.50 |
| YOLO26s pose | `[1, 56, 8400]` | Same pose layout and thresholds |
| YOLOv8n smoke/fire | `[1, 6, 8400]` | XYWH and scores for smoke and fire, confidence 0.20, class-aware NMS at IoU 0.70 |

The layouts come from the locked reconstructed graphs. The fire exporter restores the activation override at `model.9.cv1.act` through the original checked loader. The default legacy deployment uses the balanced pose weights. Rectangular `auto=True` preprocessing in the Python predictor remains a separate comparison from the initial square CPU profile.

## Checked platforms

| Platform | ORT CPU FP32 | Native video |
| --- | --- | --- |
| Windows x64 | Official ORT 1.31.0, all three graphs × seven fixture tensors | FFmpeg 8.1.2 shared build, 1,578 H.264 frames at 1920×1080 with increasing native PTS, loopback read deadline |
| Ubuntu 24.04 x64 under WSL | Official ORT 1.31.0, same locked graphs and fixtures | Ubuntu FFmpeg 6.1.1, same 1,578 frames, loopback read deadline |

The tracked evaluation reports record raw tensor errors and threshold crossings. Four inputs are frames from the credited 0.5.0 demonstration and three are deterministic synthetic tensors. These qualify the static tensor path on this host. Holdout accuracy, the legacy rectangular preprocessor, temporal state, target hardware performance and distribution licensing each have separate acceptance criteria.

## Firebase boundary

Cloud-only Node and on-device-capable Chrome use the official Firebase JS SDK. The Node service and Chrome page are independent of the Companion. The project observed in Firebase Console is on Spark and the Gemini Developer API setup requires web app registration and App Check. Successful authenticated inference, on-device model availability and lifecycle remain open. [Firebase Web hybrid setup](https://firebase.google.com/docs/ai-logic/hybrid/web/get-started)
