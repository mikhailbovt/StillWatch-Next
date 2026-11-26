# FURG flame replay

Ubuntu CPU replayed 15 original FURG videos through the locked rectangular ONNX pack and `local-policy-2`. The dataset revision is `6aff09d7baa27425f31d1df685284c26cbffe7af`. All video and XML hashes were verified against the pinned Git tree. The evaluation used no cloud routes. [FURG dataset and CC0 terms](https://github.com/steffensbola/furg-fire-dataset/tree/6aff09d7baa27425f31d1df685284c26cbffe7af)

The manifest was frozen before inference. Positive clips used the timestamp of the first run of eight consecutive annotated flame frames. Original frame indices were mapped to decoded presentation timestamps. XML and decoded frame counts can differ by a few trailing frames. The match window extends from two seconds before the label through eight seconds after. `case2_outside.mp4` contains no authored flame boxes and remains an unlabeled context clip.

| Group | Clips | Result |
| --- | ---: | --- |
| Annotated flame onset during recording | 4 | 3 matched, median delay 0.77 s, 2 duplicate reviews in the match window |
| Flame visible in the first frame | 8 | 8 matched, median delay 0.53 s, 3 duplicate reviews in the match window |
| Ordinary barbecue flame | 1 | 2 reviews without a configured region |
| Patrol robot without annotated flame | 1 | 0 reviews |
| Unlabeled exterior context | 1 | 2 reviews, ground truth undetermined |

`house1.mp4` was the missed onset. The positive videos include moving viewpoints, visible fire at several scales and correlated views. The results describe these clips. The short negative footage cannot estimate false-alert rate and the set does not qualify early smoke detection.

Manifest SHA-256 `71bf1e4b0dca362d824979e3e254356f335218e41f78cda25a24b47c720b57a0`. Raw report `.local/qualification/holdout/furg/ubuntu-rect-report.json`, SHA-256 `0dd0b405efae71f44efb28da09546f83c45c793b73c7eecf82b55a199c0103f6`. Use `tools/benchmark/evaluate.py` with the local manifest, original files and four replay lanes to repeat.
