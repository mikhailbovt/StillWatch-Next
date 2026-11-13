# Controlled flame replay

The four source recordings show a candle, two fireplaces and a wood grill in normal use. A reviewer drew the expected enclosure in each view before either runtime processed the clips. The replay compared Rust with and without these regions using the same four clips and four lanes. Python 0.5.0 provides the original candidate count. All inference stayed local.

| Source | Seconds | Rust reviews with zone | Rust reviews without zone | Python candidates |
| --- | ---: | ---: | ---: | ---: |
| [Blue candle, Jahobr](https://commons.wikimedia.org/wiki/File:Blue_candle_burning.webm) | 24 | 1 | 1 | 1 |
| [Mount Rainier hearth, National Park Service](https://commons.wikimedia.org/wiki/File:Mount_Rainier_Holiday_Hearth.webm) | 240 | 11 | 20 | 5 |
| [Fireplace, Fastily](https://commons.wikimedia.org/wiki/File:Fireplace_1_2021-02-14.webm) | 15 | 3 | 5 | 4 |
| [Wood grill, Georges Seguin](https://commons.wikimedia.org/wiki/File:Grilled_entrecote_20190119_2.webm) | 60 | 0 | 1 | 0 |

Across 339 seconds the configured regions reduced Rust reviews from 27 to 15. The legacy run opened 10 candidates. The 14 configured fireplace reviews carried the local reason `visible_fire_or_smoke_growth`. The configured candle produced one `visible_flame` review. No run produced an urgent Rust incident. Visible flame in the designated area was expected. Individual reviews were scored by opening count without independent smoke or spread labels. The sample is too small to estimate field rates.

The manifest records source attribution, licensing, source digests, trim intervals and normalized regions. Transcodes removed audio and limited width to 960 pixels. Original and transcoded clips remain local.

Manifest SHA-256 `c8bbe358d250d9a4fb3d9666adf6b986c1923b0a5d1a789b642534b9c084eab4`. Raw report `.local/qualification/holdout/controlled-fire/windows-report.json` SHA-256 `210cca0b04f493825d15c4e6f7ea1f6977c8ba6abdceda756679155f4ed0d122`.

## Growth comparison on Ubuntu

The updated policy compares measured areas of the same class. A gap longer than 1.5 seconds ends its trend. Four measurements spanning at least 1.2 seconds are required. The detector weights, confidence thresholds, video, zones, and worker build stayed fixed. Four paced lanes ran with three ONNX Runtime threads per lane. Counts varied between runs because the runtime selects the most recent frame at each inference opportunity.

| Replay | Reviews with zones, previous → updated | Reviews without zones, previous → updated |
| --- | ---: | ---: |
| Four controlled scenes, first run | 19 → 10 | 26 → 26 |
| Four controlled scenes, repeat | 17 → 13 | 23 → 33 |
| Two FIRESENSE fireplaces, first run | 7 → 8 | 8 → 9 |
| Two FIRESENSE fireplaces, repeat | 9 → 9 | 10 → 12 |

All four FIRESENSE runs matched 8/8 clips with visible flame and 10/10 with visible smoke in their original eight-second windows. The median flame delay ranged from 1.00 to 1.25 seconds and differed by at most 0.25 seconds per pair. The median smoke delay ranged from 0.75 to 0.95 seconds and increased by 0.17 and 0.03 seconds. The archive-negative clips produced 20 → 20 and 22 → 21 reviews. These development clips quantify a reduction on one controlled dataset with mixed results across scenes. The frozen MmodalFire early-onset result remains a separate acceptance gap.

The Core SHA-256 values were `a06aba6d11492a541acc4ebe71b45451c7c3fbf33d95a7c4764636f51595fbd6` and `1e80cbbfb926ece7490042faaff42e382a601edf7587ce88066ba0308ed94752`. Both used worker `8faf3f56d3d5d92cbe856394e6564b7488bed3ea722767d844484fcb1ada2468`. Raw reports are in `.local/qualification/fire-growth-comparison` and `.local/qualification/fire-growth-repeat`. Repeat with `tools/benchmark/evaluate.py` using the same manifests, model pack and `--parallel 4`.
