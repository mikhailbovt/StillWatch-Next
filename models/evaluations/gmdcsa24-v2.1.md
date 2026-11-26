# GMDCSA-24 fall and daily activity replay

This Windows CPU holdout covers four volunteer actors and three home environments. Three annotated falls and four daily activities per actor were selected before scoring. The manifest pins the original v2.1 videos, their published CSV annotations, and the evaluation window. Source media remains local. [GMDCSA-24 dataset v2.1](https://doi.org/10.5281/zenodo.13354453), [data paper](https://doi.org/10.1016/j.dib.2024.110892)

| Metric | Rust Core | Python 0.5.0 |
| --- | ---: | ---: |
| Falls matched | 12/12 | 12/12 |
| Median delay from annotation | 0.491 s | 0.514 s |
| Additional matches in the same fall window | 1 | 1 |
| Daily-activity medical reviews or candidates | 10 across 16 clips | 10 across 16 clips |
| Daily-activity urgent incidents | 0 | No comparable urgency score |

The first same-category event from 0.35 seconds before the annotated fall to eight seconds afterward counted as a match. Each runtime reviewed the same ten daily-activity clips. These include lying down on a bed or floor, exercising and picking objects up from the floor. The daily-activity footage totals 134.84 seconds, so its count describes this replay only. The volunteers performed staged falls. The result does not establish performance for unscripted falls, assisted recovery or care environments.

Manifest SHA-256 `8d7a45caf43af90ac7dc5188988f361155381430c7f70f0b4bc6407ab5ba2c1c`. Raw report `.local/qualification/holdout/gmdcsa24/windows-report.json` SHA-256 `d068d37432f36db89d8fe83651663b261645aa6c8507b1868a38570b46adac46`.
