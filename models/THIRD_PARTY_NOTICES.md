# Model origins and licenses

The model lock in this bundle records the source checkpoints, their digests, export configuration, ONNX digests and parity fixtures. The same model licenses apply to the exported weights.

| Export | Source checkpoint | Upstream license |
| --- | --- | --- |
| `pose.onnx` | Ultralytics YOLO26n Pose from release v8.4.0 | [AGPL-3.0](https://www.ultralytics.com/license) |
| `pose_balanced.onnx` | Ultralytics YOLO26s Pose from release v8.4.0 | [AGPL-3.0](https://www.ultralytics.com/license) |
| `fire.onnx` | rabahdev/fire-smoke-yolov8n, revision `13017fe8af477c25f5298d168e2dfede4b000753` | [AGPL-3.0 as declared by the model publisher](https://huggingface.co/rabahdev/fire-smoke-yolov8n) |

The fire model's pinned metadata has no training dataset inventory. StillWatch Next tracks permissions for evaluation footage separately in each corpus manifest. Any public bundle that includes these weights requires the distribution review recorded under G4 in the release matrix. See the [GNU AGPL v3 text](https://www.gnu.org/licenses/agpl-3.0.html).
