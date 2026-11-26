# MmodalFire laboratory replay

The Windows CPU replay used the locked rectangular FP32 model pack, four concurrent lanes, and three ONNX Runtime threads per lane. The original StillWatch 0.5.0 ran against the same clips. Both runs stayed local. The manifest was frozen before either runtime saw the clips. It hashes the dataset, source videos, sensor timestamps and selected footage. [MmodalFire paper](https://doi.org/10.1038/s41597-026-06810-6), [dataset v1 and CC BY 4.0 terms](https://doi.org/10.6084/m9.figshare.28804448.v1)

| Footage | Exposure | Rust Core | Python 0.5.0 |
| --- | ---: | --- | --- |
| Cotton, heptane and polyurethane | 6 onsets | 0/6 within the window, 5 late reviews | 0/6 within the window, 4 late candidates |
| Wood smoke | 2 onsets | 0/2 within the window, 1 late review | 0/2 within the window, 1 late candidate |
| Dry ice | 1,810 s across 4 clips | 0 reviews and 0 urgent incidents | 0 candidates |
| Water mist | 1,755 s across 4 clips | 7 reviews across 3 clips and 0 urgent incidents | 2 candidates across 2 clips |

The match window starts two seconds before the first visually labeled flame or persistent smoke and ends eight seconds after it. A single reviewer labeled onsets in sampled stills. Sensor worksheets locate the recorded intervals. They do not supply the visual onset labels. The six late Rust events opened 31.16–68.92 seconds after their annotated onset. Positive clips had 149–167 sampled fire inferences each. Per-clip samples with analysis unavailable or warming up accounted for at most 0.34 percent. Replay never queued alert delivery.

All sixteen clips share one laboratory camera and room. The negative exposure is 0.9903 camera-hours. Correlation across trials and the short exposure prevent an operational false-alert estimate. The missed early detections and extra mist reviews leave the current fire profile unqualified for the quality gate. No model threshold or policy was tuned on these results.

## Detector observations around onset

A second local pass sent timestamped RGB frames from the native capture worker directly to the production Windows inference worker. The eight clips contributed 84 samples each in the 18–28 s interval. Sampling used a minimum spacing of 100 ms, which selected every third frame at the source's 25 fps. The resulting detections already include the locked score threshold and non-maximum suppression. This pass bypassed the Core scheduler and incident policy.

| Clip | Samples with a detection | Highest score | First detection after onset |
| --- | ---: | ---: | ---: |
| Cotton 1, cotton 2, polyurethane 2, wood 1, wood 2 | 0 each | — | — |
| Heptane pair 1 | 3 | 0.334 | 1.72 s |
| Heptane pair 2 | 3 | 0.424 | 5.08 s |
| Polyurethane 1 | 1 | 0.262 | 1.36 s |

Five clips yielded no observations at the sampled times. The remaining three yielded seven weak observations total. Those results limit what the policy can recover with the same detector. The diagnostic cannot establish coverage between samples or the effect of a lower threshold. Repeat with `tools/benchmark/fire_observations.py`. The detailed trace is `.local/qualification/holdout/mmodal-fire/windows-observations-onset.json`, SHA-256 `1d14fb1e323cf66a9985d8a9ece1d3d072891d085fab802b606bd879479672eb`.

Manifest SHA-256 `4731d9caedb1bc51ab8dcd297377ec0af0f753cf7bf85d28a23631db8f34c7f1`. The raw per-clip report is `.local/qualification/holdout/mmodal-fire/windows-report.json`, SHA-256 `3b5afc887bfbcbfbcfd1cca2842cd3a086b1580e7abb5c026b42b1fd71972cba`. Use `tools/benchmark/evaluate.py` to repeat the comparison with the manifest and identical media.

## Full follow-up on Windows Core `3c04f65`

The MSI payload ran the eight original 90-second positive clips through four CPU workers with three inference threads each. The visual onset is at source time 20 s in each clip. All eight had 89.52 s of verified fire coverage, including each complete matching window. Core opened zero Reviews within the eight-second deadline. Three clips opened a later Review. The saved opening rectangles cover visible flame on the corresponding source frames.

| Original clip | First Review after visible onset |
| --- | ---: |
| Heptane pair 1 | 21.56 s |
| Polyurethane 2 | 51.60 s |
| Cotton 2 | 57.56 s |
| Cotton 1, heptane pair 2, polyurethane 1, wood smoke 1 and 2 | No Review before clip end |

The earlier 32-second test on `b538551` used a separate 640×360 transcode at 15 fps. Its one timely Review came from heptane pair 2. The full follow-up used the original 1920×1080 frozen replay files, so the comparison includes differences in frame content and timing as well as binary revisions. Both tests use one laboratory camera.

The full report is `.local/qualification/completion-step-12/windows-3c04f65-mmodalfire-full-positive-windows/replay.json`, SHA-256 `26ff1a03677d09f04d54f7e37550e3eb575363b4153d1d53b4fe2baef2f0c498`. Its `late-frames/index.json` links the three reviewed frames to the immutable openings and records their digests, SHA-256 `da038fe5aa3052ec4b204ea7e17f7f69a84a087ac58d1f7e121be1ae3d5f25ca`.

## Frozen replay with the revised growth calculation

An Ubuntu CPU run of `local-policy-2` used the same frozen manifest with no threshold tuning. It again matched 0/8 early flame or smoke onsets. Three late reviews appeared in the six flame clips. The two wood smoke clips produced no review. Water mist produced five reviews in 1,755 seconds. Dry ice produced none in 1,810 seconds. No negative clip produced an urgent incident. Differences from the earlier Windows run combine the policy change and the runtime host. The early-onset criterion remains unmet.

Report `.local/qualification/fire-growth-repeat/mmodalfire-frozen-policy2.json`, SHA-256 `e01bb4028a2eb0add870af1e17ea03e0179199e663a70233079b1d6008ee9c3e`. The report records the Core, worker, ONNX Runtime and model pack digests.

## Pre-threshold development trace

On Windows the legacy and diagnostic commands returned identical observations at all 672 sampled PTS, also matching the saved legacy run. The diagnostic records the 16 highest-scoring anchors for each class. A reviewer marked the approximate search ROI for each clip before inspecting those anchors. A matching diagnostic proposal needs IoU of at least 0.10 and score of at least 0.05.

| Clips | Earliest stage observed in the selected windows |
| --- | --- |
| Cotton 2, polyurethane 1, wood 1 | No matching proposal at or above 0.05 among the recorded anchors. |
| Cotton 1, polyurethane 2, wood 2 | Matching proposals below the legacy score cutoff. Peak scores 0.17024, 0.07751 and 0.08044. |
| Heptane pair 1 | A matching proposal scored 0.26477 and was suppressed by NMS at 26.64 s. No matching box survived in the selected window. |
| Heptane pair 2 | A matching Flame box scored 0.29607 at 25.08 s, 5.08 s after the label. On those same sampled frames, the local policy reports good visibility, one continuity sample and no strong detection or matching Review. The separate paced pass has no timely durable opening. |

These approximate ROIs are for development attribution and are shared by all sampled frames in a clip. The trace covers the recorded top anchors and selected times. The same-frame policy trace bypasses live scheduling and storage. Replay does not produce live alert intents. Diagnostic `.local/qualification/completion-step-1/mmodalfire-diagnostic.json`, SHA-256 `1b06f99263fb3379128f257198d5f80a134a20b18936a21d267649627b7535fb`. The policy trace with the same RGB hashes and raw proposals is `.local/qualification/completion-step-1/mmodalfire-policy-trace.json`, SHA-256 `b408455ff2e2ed4109400dcacecdd1bae90057bf6d6b187004edade4de1f4986`. ROI definitions are in `mmodalfire-v1.regions.json`.
