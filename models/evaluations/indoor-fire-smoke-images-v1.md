# Indoor Fire Smoke image transfer

The Windows inference worker used the locked rectangular CPU FP32 model pack. This check measures detector observations on still images. It uses the normal worker's preprocessing, class scores, thresholds and non-maximum suppression over binary IPC. The source images were decoded to RGB by Pillow 12.2.0. The test stayed offline. [Zenodo dataset and CC BY 4.0 metadata](https://doi.org/10.5281/zenodo.15826133), [Original export](https://universe.roboflow.com/object-detection-7qn6l/indoor-fire-smoke/dataset/1)

The test split contained 750 images. Before inference, the manifest excluded 233 whose filename stem before `.rf.` or ZIP CRC matched an image in the provider's train or validation split. It excluded seven more by repeated stem or identical image SHA-256 within the test split. A review of 12 examples per numeric class identified `0` as flame and `1` as smoke. The latter includes stage fog and humidifier vapor. Greedy one-to-one matching used the same class and IoU ≥ 0.5. A small annotation occupies less than 1% of image area.

| Class | Matched boxes / annotations | Small boxes matched / annotated | Images with a matched box / annotated | Images with any prediction of this class / annotated |
| --- | ---: | ---: | ---: | ---: |
| Flame | 190 / 425 | 43 / 117 | 162 / 298 | 202 / 298 |
| Smoke | 33 / 294 | 0 / 38 | 32 / 249 | 102 / 249 |

The broader image-level counts include predictions at the wrong location. Unknown sourcing and visually similar frames limit independence. The dataset supplies neither real onset timestamps nor representative negative-only exposure. These measurements describe perceptual transfer only. The model and alert policy were left at their locked settings.

The original archive, frozen manifest, method and per-image boxes are under `.local/qualification/holdout/zenodo-indoor-fire-smoke-15826133`. Archive SHA-256 `f5e4b452bc0e74508366b4ce3f62d7479d1a790a7438a1f0ebee145a216d7b6a`, manifest `dba7f1668bc96b8e0928f1b4aa694810503422a67a7c34308d23a3bf6bf13d11`, report `fc2588574bfd37b471cba2ce038e9312201cb7829fe626272c1e1ab0390ab8db`. The images stay outside version control.
