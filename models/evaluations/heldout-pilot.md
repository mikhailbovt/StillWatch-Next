# Independent event replay pilot

The frozen input manifest was created before either runtime processed the 23 clips. It hashes the videos, annotation inputs and scoring window. The Windows CPU result records the Core and worker digests, the pinned legacy replay source and per-clip observations. Media and source annotation files stay outside the repository. The pilot ran with cloud processing disabled and no notification intents.

| Scenario | Inputs | Rust Core | Python 0.5.0 |
| --- | ---: | --- | --- |
| Staged falls | 9 clips across two held-out subjects | 9 matched, 0 duplicates, median delay 0.82 s | 9 matched, 0 duplicates, median delay 0.82 s |
| Activities of daily living | 10 clips, 111.8 s | 5 medical review incidents | 5 medical candidates |
| Annotated flame | 2 clips | 1 matched within 8 s, 5 later events | 1 matched within 8 s, 1 in-window duplicate, 6 later events |
| Configured barbecue | 1 clip, 30.1 s | 4 review incidents with the initial zone. The paired run without the zone also produced 4. | 3 fire candidates |
| No-flame control | 1 clip, 11.4 s | 0 fire events | 0 fire candidates |

CAUCAFall contributes one fixed camera view and two held-out subjects. The medical observations include false reviews during kneeling, sitting, hopping and picking up an object. Rust and Python used the same source clips and locked model weights. Rust processed the clips at the source cadence through the native scheduler. Python used the original deterministic replay and its local candidate timestamps. The Python candidates carry no operational urgency score in this comparison. [CAUCAFall v4](https://data.mendeley.com/datasets/7w7fccy7ky/4), [OmniFall cross-subject evaluation](https://dsch.ai/omnifall/)

The FURG annotations mark flame boxes. They provide no independent smoke or hazard-severity labels for these clips. The barbecue shows visible smoke and changing flame shape, so review remains a plausible conservative response. Its count cannot establish a false-alert rate for benign cooking. The two fire clips cover a narrow scene sample. The negative exposure is too short for a rate claim, and episodes from the same subject or scene are correlated. The intervals in the raw report are descriptive. [FURG Fire dataset and annotations](https://github.com/steffensbola/furg-fire-dataset/tree/6aff09d7baa27425f31d1df685284c26cbffe7af)

`tools/benchmark/evaluate.py` replays the manifest against Core and optionally the original checkout. It verifies the video hashes and reads the source timestamps of immutable incident-opening records. The 8-second comparison window counts the missed clip as a miss even when a later observation opens a review. The result leaves fire timing, false reviews and independent operating-profile acceptance open.
