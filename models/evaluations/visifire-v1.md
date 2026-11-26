# Bilkent VisiFire transfer replay

The Ubuntu CPU run used the locked rectangular FP32 model pack, three concurrent replay lanes and three ONNX Runtime threads per lane. A reviewer labeled the clips before running inference. The manifest pins original and normalized media, the time windows and the exclusions. Original dimensions and cadence were kept. Audio was removed. Media remains local. [Bilkent VisiFire](https://kilyos.ee.bilkent.edu.tr/~signal/VisiFire/)

| Footage | Review time in the clip | Interpretation |
| --- | ---: | --- |
| Outdoor patio, fire1 | 9.87 s | The first clear flame appeared between the 1 s and 2 s stills. Delay is between 7.87 and 8.87 s. The nominal 2 s label passes the eight-second window, but annotation uncertainty spans the boundary. |
| Distant experiment, 40m Pan Fire | 2.44 s | A small possible flame is present from the first still. The clip ends at 4.67 s. |
| Four views with visible forest flames | 0.60–1.13 s | All four matched. They produced five additional reviews inside the window. The original source or scene can overlap across views. |

One earlier FIRESENSE clip visibly duplicates the VisiFire controlled1 scene. Four further forest views were excluded for overlapping framing or plume. The remaining watermarked forest views were grouped as correlated. All six clips produced a total of eleven fire reviews. Replay queued zero deliveries, and the sample contained no negative exposure or indoor onset. The outdoor patio result warrants a scene-specific timing report. Indoor smoke sensitivity stays open.

The manifest and report are in `.local/qualification/holdout/bilkent-visifire`. Manifest SHA-256 `5b72c40579b7f5638443653b7022c053c2445d7012e27a56f6a938d3077795ee`, report SHA-256 `6ee61f0b16c4df871d462be32c15e93888513ec409351531c8d113423321da6e`. Repeat with `tools/benchmark/evaluate.py` and the same media, runtime and manifest.
