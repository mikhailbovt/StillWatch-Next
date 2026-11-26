# StillWatch Next Hybrid Web walkthrough

StillWatch Next monitors the cameras attached to a Windows or Linux host. The Rust Core runs the local ONNX models and saves incidents with masked evidence. The Hybrid Web worker uses Firebase AI Logic to describe selected evidence. Operators can see the route reported by the Firebase SDK before deciding how to act on the description.

## Review environment

Use a dedicated review host or a disposable data directory, the Core payload and matching model pack from one build, and a supported Chrome profile with the local model installed. The reference walkthrough uses Windows x64, Chrome 157, Node 24 and Firebase JS SDK 13.3.0. Follow the [installation guide](operations.md) and the [Hybrid Web setup](firebase-worker.md#build-and-configure). Configure a dedicated Firebase test app with App Check and separate credentials for Core and Chrome. Use the credited replay from [baseline.json](../baseline.json).

1. Open the loopback worker page in the dedicated Chrome profile. Connect and wait for local model readiness. With the model installed, the recorded cold starts took about 156 seconds and 357 seconds. The longer run recovered after two warmup retries.
2. Start a replay evaluation using `local_only` with cloud consent disabled. Use the [credited replay procedure](firebase-worker.md#verify) to send current Core evidence. Open the record in the worker page and check the incident ID, evidence version and actual `on_device` source. The replay runner also checks the saved result and the absence of live delivery for the replay.
3. Run `prefer_local` with the browser model unavailable and cloud consent enabled on a test camera. Check the actual `in_cloud` result and repeat the consent check with the private camera. The fixture runner checks that Core sends zero cloud submissions for that camera.
4. Return to the local route. Freeze and resume the worker tab in the dedicated profile. Then discard the tab and load it again from `chrome://discards`. In each case verify that the lease expires while the tab is unavailable and a new evaluation returns `on_device` after recovery. The recorded discard returned to ready in 4.22 seconds from reload. The [acceptance matrix](acceptance.md) lists the observed coverage and the remaining App Check checks.

## Web submission scope

The Web route runs Firebase AI Logic and Gemini Nano in Chrome. The worker sends current masked evidence through `generateContent` and shows the SDK's `inferenceSource` beside the Core incident and evidence version. It can route a distinct, consented request to cloud inference. Core provides camera ingest, local detection and incident persistence. The Companion uses Core's paired API for video and incidents. This is the basis for selecting **Web (Chrome)** under section 6 of the contest rules. [Firebase SDK call](../packages/firebase-worker/src/generate.js)

Section 3 excludes residents and organizations domiciled in the listed jurisdictions, including Russia. The submission window closes on November 30, 2026 at 2:00 p.m. Pacific Time. Eligibility for a specific entry remains with the entrant and organizer. [Official rules](https://firebasehybridaihackathon.devpost.com/rules)

StillWatch Next builds on the Apache-2.0 StillWatch 0.5.0 release pinned in the baseline. The existing project supplies the model and decision reference, the historical implementation for parity, and credited demonstration footage. During the submission period, the November 13 implementation introduced the standalone Rust Core, independent model processes and Companion clients. The current project integrates Hybrid Web inference with versioned masked evidence, local and consented cloud routing, and recovery after loss of the browser session. [Baseline and footage credits](../baseline.json) · [Verification record](../IMPLEMENTATION_STATUS.md)

## Demonstration footage

| Time | Show on screen |
| --- | --- |
| 0:00–0:30 | A masked camera view and an incident in Companion. Explain that Core owns the capture, local detection and durable incident. |
| 0:30–1:15 | The same incident ID and evidence version in the Chrome worker. Show the local-only route, cloud consent disabled and the actual `on_device` result. Keep the overview visible while the response is fresh. |
| 1:15–1:55 | A second test camera with explicit consent. Show the actual `in_cloud` result with the local model unavailable and the protected camera's absence from the cloud submissions. |
| 1:55–2:40 | Close the worker while Core continues analyzing a live test feed. Show the Pose and Fire progress in Core and then return to the worker. |
| 2:40–2:55 | Show where to find the install instructions, runtime code and credits. |

Use English narration or English subtitles. Record the actual application and retain source and evidence credits. Crop secrets, device credentials and local private paths from the recording. The [official rules](https://firebasehybridaihackathon.devpost.com/rules) govern submission eligibility, access for judges, the public source repository and the demonstration requirements. The proposed platform is Web (Chrome) for the Hybrid Web worker. The treatment of the separate Rust Core and the rights to redistribute the bundled weights remain open in the [release matrix](../qualification/release-requirements.json).

## Recorded build review

The 110-second English review uses separate Windows Companion QA captures and recorded Hybrid Web results on Chrome 158.0.8127.36. Windows Core `20c8a0d` persisted the local and cloud evaluations of the credited replay. Each Chrome panel shows the corresponding incident, evidence version and masked overview. In the cloud trial, the restricted camera opened one local incident and generated zero cloud attempts.

With the Chrome worker disconnected, the Core accepted 32 Pose and 12 Fire observations during a 6.203-second sample on a live RTSP source. After reconnecting, the browser completed a new local-only replay evaluation, which the Core persisted. The MP4 hash, source credits and capture evidence accompany the local review.
