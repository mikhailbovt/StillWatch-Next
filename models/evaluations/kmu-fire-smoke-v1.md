# KMU indoor smoke replay

Ubuntu ran the locked rectangular CPU ONNX pack and `local-policy-2` on both indoor smoke clips and ten clips from the source's smoke- or flame-like object archive. The authors provide the recordings for research and request citation. The downloaded media stays local. [Keimyung University Computer Vision and Pattern Recognition dataset](https://cvpr.kmu.ac.kr/Dataset/Dataset.htm)

A single reviewer froze the manifest after inspecting sampled frames and before running inference. The daytime plume is visible at the first sampled frame. The low-light sequence uses the same room and has a camera menu overlay. The last sampled frame before the clear plume is at 32 s, the first clearly visible plume is at 34 s, and it persists at 36 s. The match interval extends from two seconds before the label through eight seconds after it. The low-light recording ends at 39.33 s, so it covers 5.33 of the eight seconds after its label.

| Group | Clips | Fire results |
| --- | ---: | --- |
| Low-light smoke onset | 1 | No event. The clip recorded 74 fire frames and 185 pose frames at its highest sampled counters. |
| Visible daytime smoke at start | 1 | One late review at 13.67 s. The clip recorded 49 fire frames and 121 pose frames at its highest sampled counters. |
| Source-labeled lookalikes | 10 | 13 reviews on five clips over 0.2953 camera-hours. No urgent fire events. |

The ten comparison clips carry archive-level labels. One of them also produced a medical review, which is outside the fire tally. The shared setting, short exposure and partial low-light window limit the result to this replay. The run did not change model weights or thresholds.

Core SHA-256 `be93d2b93d53c42755b6c5bafcadc0a7e4d6df7a7991e91651711edff6c59aaa`, worker `e6291bd2a1bc00d2e191e5169d97b1bf7078d0d46860fcac59957fa6fdb99070`, model manifest `b1389a94fc0c90d68a49277906e1a4c8b427217b6b2301883b9c0f5dfcb23a8c` and ORT `469849977cb60744ea160927e1a3a0643fd4c55c0aff743251c6d293402e334e`. The run used four replay lanes with three inference threads per lane. The manifest and raw report are under `.local/qualification/holdout/kmu-fire-smoke`, with SHA-256 `e1836c81374f8b7a321d9c912655ec98f3b3f3427aeda124292e23408cf60426` and `cd3f334261440acd13b845fcdae2d041ea7098b95838db48a48685a638db8da7`. Use `tools/benchmark/evaluate.py` with the staged recordings and manifest to repeat the measurement.

## Windows coverage and pre-threshold trace

Windows replay measured Fire coverage throughout the daytime clip's first eight seconds. The first recorded Review is at 9.708333 s from clip start. The low-light clip remains right-censored. The ten comparison clips produced 13 fire reviews over 0.29426 verified camera-hours. Three comparison clips fell below the 99% per-clip coverage floor. This short development exposure is insufficient for a release false-alert claim. Report `.local/qualification/completion-step-1/kmu-windows-native.json`, SHA-256 `90fbbc01e2456318d761809b86f48e0863ae5ef4fa7b1591ae2f42cc6577be3b`.

The Windows diagnostic sampled 65 daytime and 75 low-light frames. It emitted a matching daytime smoke box at 3.375 s with score 0.36196. The low-light plume's highest matched score was 0.07245, below the legacy cutoff. Search regions are in `kmu-fire-smoke-v1.regions.json`. The paced Core run may select other frames. Diagnostic `.local/qualification/completion-step-1/kmu-diagnostic.json`, SHA-256 `a6f08a0216919eb7d7d8ac673e861c156c4c11a60d2cf4e827b2b597113e09a5`.

The default unreviewed-scene policy consumed the identical sampled frames and observations. It returned Review for the daytime smoke at 3.875 s on the second matching sample. This isolates what the policy could do on this sampling. It does not establish the cause of the later opening in the paced run. Policy trace `.local/qualification/completion-step-1/kmu-policy-trace.json`, SHA-256 `d5d06a6e81fb15d97a5efd4a402dcd6b832c416e0514021b6276998a8e51afe7`.
