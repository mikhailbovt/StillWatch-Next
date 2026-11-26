# UP-Fall everyday activity and NIST cooktop re-ignition

These development replays use the packaged Windows CPU Core and worker from `b538551`, the 4:3 FP32 model pack (`bdc4fc0a24490d6ac54fe1345c9002d44ad9826edd491f31e370a304be3ba4b3`) and three inference threads per worker. The source manifests, subject annotations, eligibility windows and rights records were frozen before the first model run.

## Falls and ordinary activity

Three participants from the WCCI training split each contributed one staged fall and one ordinary activity. The six trials share a laboratory and camera. The replay preserved all 2,589 source PNGs and their relative timestamps. The positive clips have 503 frame-level subject annotations, including other people visible in the scene. [UP-Fall authors](https://sites.google.com/up.edu.mx/har-up/) · [Dataset and activity definitions](https://www.mdpi.com/1424-8220/19/9/1988) · [Frozen manifest](upfall-camera1-six-trials-v1.manifest.json)

| Participant | Last negative frame (s) | First visible fall (s) | First medical Review (s) | Actor and opening |
| --- | ---: | ---: | ---: | --- |
| 2 | 1.094966 | 1.439392 | 1.641933 | Same actor, suspected fall |
| 6 | 1.200852 | 1.540273 | 1.461058 | Same actor, suspected fall inside the onset bracket |
| 10 | 0.847789 | 1.207761 | 1.427350 | Same actor, partial body descent |

Each clip contains the entire eight-second post-onset window. Each generated exactly one medical Review. Sampling the API every 250 ms left gaps in observed pose coverage, so the original scorer returned three `invalid_coverage` windows. Its 0.35 s pre-onset allowance and eight-second deadline were identical for all three clips.

The three ordinary activities contained 129.768 s of footage and 80.605 s of verified medical exposure. Picking up an object generated one suspected-fall Review at 1.828466 s. Lying down intentionally generated two Reviews, for loss of view at 33.633852 s and persistent low posture at 59.126408 s. All three openings fell inside verified exposure. Sitting generated zero medical openings and one smoke Review at 57.597232 s. These short development recordings do not establish a field false-alert rate.

The original report is `.local/qualification/completion-step-12/upfall-native-development-v1/cpu4-events.json`, SHA-256 `2ac5fb96c7f1e36c4de2fa6b7bef1c3ecc28b18a16a606cff626ba54e44a1f58`.

On a second pass, the collector waited 40 ms between polls and saved each response's monotonic timestamp, task counter and source timestamp. Downsampling those same responses to 250 ms concealed accepted frames. With the same scoring rules, the full responses verified 101.799 s of medical exposure compared with 89.631 s from the downsampled responses. All three positive windows still contained at least one interval below the required single-frame rate, and the same three false Reviews remained. The paired receipt is `coverage-resolution/paired-results.json`, SHA-256 `73e6c65e143f90811186c14c03d9252fb88461544c5deeb64523ef38473501c0`.

Native pose output for the pickup showed two visible legs with confident landmarks. The ankles stayed within 4% of the earlier box height, while the hips dropped about 12–16%. One true backward fall also had two confident legs at the opening. A standing-bend exception needs more discriminating support than ankle position alone. The observation receipt is `coverage-resolution/pose-result.json`, SHA-256 `7e340b019f0c2a833888b79e2c9f68b87e089947ca0af2ed3d38855866d0cd7d`.

## Re-ignition after suppression

The NIST recording contains an existing oil fire, a pan obscured by suppressant and a visible return of flame at the same pan. Suppression occupies part of a positive recording and contributes no negative exposure. The source annotation brackets re-ignition between 148.5 and 148.9 s. It fixes both the pan region and the requirement for visible flame on the opening observation. [NIST recording](https://www.nist.gov/video/cooktop-reignition-oil) · [Frozen manifest](nist-tn1969-cooktop-reignition-v1.manifest.json)

The ordinary replay produced one smoke Review at 68.066666 s. The opening described smoke across the upper part of the frame. Core analyzed the entire 146.5–156.9 s matching window and opened zero incidents for the returning flame. In a second replay, the local operator closed the smoke incident through the authenticated API at source time 144.066666 s. Core recorded that resolution, completed all 326 fire samples and opened zero subsequent incidents. Both replays use the same recording.

The ordinary report is `.local/qualification/completion-step-12/nist-cooktop-reignition-v1/cpu4-events.json`, SHA-256 `e0a1d444add9e2ddd0c8c8e5df4fa25ea682c6900b4e54b96a88863b3471d7fb`. The operator run is `operator-run/result.json` in the same directory, SHA-256 `627b590e6b7a4f43e55ffd96a0161ead2c319deae22dd36c4d816ceb1585324d`. Replay queued zero notifications. SQLite integrity and foreign keys passed.

At the exact 21 source timestamps observed during the operator run's matching window, the production and diagnostic worker modes agreed on all accepted regions. Every timestamp yielded a smoke detection somewhere in the scene. That window yielded zero accepted or weak flame proposals at the pan. The highest flame score among the bounded raw proposals that overlapped the pan was 0.00359, below the production weak-proposal floor of 0.12. The measured miss starts at detection on these frames. The diagnostic receipt is `detector-result-v2.json` in the same directory, SHA-256 `89541a16091fa0f53e2ae32d62f7502393dd3c6f443c21ed7e66407499a482bc`.

The same 16 visible re-ignition frames were tested with fixed 960-pixel inference and four overlapping tiles, using the same weights and the 640-pixel release worker as a control. The 960-pixel pass yielded one weak flame observation at the pan and zero accepted observations. The tiles yielded one accepted and one weak observation at the pan, plus one accepted flame elsewhere on the initial-smoke control frames. Median CPU round-trip times on the 16 paired frames were 35.12 ms at 640, 84.76 ms at 960, and 144.48 ms with four tiles. The diagnostic is limited to detector output on one scene. The receipt `scale-result-v2.json` has SHA-256 `dee17a81083cb790c7a8afc1080fdea934f1ef9a14333974ba6febe5bbe245b8`.
