# FIRESENSE v1 development replay

Windows Core and the original StillWatch 0.5.0 replayed the same 45 local clips. The manifest records the source archives, hashes, selected footage, region polygons and four exclusions. The clips retain source timing. They were trimmed and transcoded to H.264 with a maximum width of 960 pixels. Nikos Grammalidis, Kosmas Dimitropoulos and Enis Cetin published the source archives under CC BY 4.0. [FIRESENSE v1](https://doi.org/10.5281/zenodo.836749)

| Footage | Exposure | Rust Core | Python 0.5.0 |
| --- | ---: | --- | --- |
| Visible flame | 8 clips | 8/8 within eight seconds, median 0.517 s | 8/8, median 0.517 s |
| Visible smoke | 10 clips | 10/10 within eight seconds, median 0.500 s | 10/10, median 0.500 s |
| Source negative for flame | 16 clips, 339.6 s | 19 fire reviews | 19 fire candidates |
| Source negative for smoke | 9 clips, 234.9 s | 3 fire reviews | 3 fire candidates |
| Two ordinary fireplaces | 2 clips, 115.2 s | 9 fire reviews with zones, 10 without | 3 fire candidates |

The positive clips begin with a visible flame or plume. The delay is measured from the first frame of the selected view. The two fireplaces were labeled contextual negatives before running either implementation. In the fireplace with eight reviews, five were attributed to area growth, two to flame with smoke and one to visible flame. The second fireplace had one flame-with-smoke review. A street scene with moving vehicles accounted for ten Rust reviews among the archive's flame negatives. Short, correlated clips and source-level labels limit the results to a development comparison of reviews and candidates.

The Rust run used four lanes with three ORT threads per lane, the locked rectangular CPU FP32 models and ORT 1.31.0. The manifest SHA-256 is `65183ef9ac1c26ce08be553a1a0e90b1a1908feddb6af87c68fb4b82bf3b6daa`. The detailed report at `.local/qualification/holdout/firesense/windows-report.json` has SHA-256 `3d982c5151ccbe541e75af1905dda812fbff85040b785b7c8a801662970bb985`. Repeat using `tools/benchmark/evaluate.py` and the frozen manifest.
