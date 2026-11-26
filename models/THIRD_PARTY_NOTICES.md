# Model origins and licenses

The [model lock](models.rect.lock.json) pins the checkpoint, export recipe and ONNX graph for each role. The following ONNX SHA-256 digests match the 0.1.0 Windows MSI and the Linux CPU and CUDA DEB and RPM packages.

| Export | Checkpoint | ONNX SHA-256 |
| --- | --- | --- |
| `pose.onnx` | Ultralytics `yolo26n-pose.pt`, v8.4.0 | `bef2f9cff3df319be3eb704126234195a6ea01ab504a2f033122d986631d3701` |
| `pose_balanced.onnx` | Ultralytics `yolo26s-pose.pt`, v8.4.0 | `1ce877d65ddff71caf04a51c16702860ffb36f2feda3113b07274ecf39a51d09` |
| `fire.onnx` | rabahdev/fire-smoke-yolov8n `best.pt`, revision `13017fe8af477c25f5298d168e2dfede4b000753` | `14dfbb5c4e6e4e6ee157dd7362febc98ab70779eb66792ae909d2a2e1a80ed44` |

The Ultralytics checkpoints match the SHA-256 digests in the v8.4.0 release. The Fire checkpoint matches the LFS SHA-256 at the pinned model revision. [Ultralytics v8.4.0 assets](https://github.com/ultralytics/assets/releases/tag/v8.4.0) · [Fire model tree at the locked revision](https://huggingface.co/rabahdev/fire-smoke-yolov8n/tree/13017fe8af477c25f5298d168e2dfede4b000753)

Ultralytics offers AGPL-3.0 and Enterprise terms. The Fire publisher declares AGPL-3.0 at the pinned revision. Treat exported weight copies as subject to their upstream terms. A public model-bearing distribution needs a documented license decision for each model and the covered work, the required notices and a corresponding-source delivery method. The Apache-2.0 license at this repository's root covers the StillWatch source. [Ultralytics terms](https://www.ultralytics.com/license) · [Pinned Fire model card](https://huggingface.co/rabahdev/fire-smoke-yolov8n/blob/13017fe8af477c25f5298d168e2dfede4b000753/README.md) · [AGPL v3](https://www.gnu.org/licenses/agpl-3.0.html)

The Fire publisher reports training on D-Fire. D-Fire's authors dedicate their collection to the public domain under CC0 and identify HPWREN, the University of Split's Center for Wildfire Research and FireNet as sources of third-party images. They refer users to the original providers' terms and request credit for the Split collection. File-level training provenance and original-image permissions remain unresolved. [D-Fire collection license](https://github.com/gaia-solutions-on-demand/DFireDataset/blob/3890a756b224855f4f7ef8d237a70bc1834a4714/LICENSE) · [D-Fire source inventory](https://github.com/gaia-solutions-on-demand/DFireDataset/blob/3890a756b224855f4f7ef8d237a70bc1834a4714/README.md#-public-dataset-images)

StillWatch Next records evaluation footage permissions in each corpus manifest. The public source tree includes locks and export tools. The existing model-bearing installers are held for the distribution decision tracked under G4 in the [release matrix](../qualification/release-requirements.json).
