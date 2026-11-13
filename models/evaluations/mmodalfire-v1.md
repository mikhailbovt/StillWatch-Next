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
