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
