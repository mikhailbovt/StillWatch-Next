# StillWatch Next Hybrid Web walkthrough

StillWatch Next monitors the cameras attached to a Windows or Linux host. The Rust Core runs the local ONNX models and saves incidents with masked evidence. The Hybrid Web worker uses Firebase AI Logic to describe selected evidence. Operators can see the route reported by the Firebase SDK before deciding how to act on the description.

## Review environment

Use a dedicated review host or a disposable data directory, the Core payload and matching model pack from one build, and a supported Chrome profile with the local model installed. The reference walkthrough uses Windows x64, Chrome 157, Node 24 and Firebase JS SDK 13.3.0. Follow the [installation guide](operations.md) and the [Hybrid Web setup](firebase-worker.md#build-and-configure). Configure a dedicated Firebase test app with App Check and separate credentials for Core and Chrome. Use the credited replay from [baseline.json](../baseline.json).

1. Open the loopback worker page in the dedicated Chrome profile. Connect and wait for local model readiness. The direct cold-start verification took about 156 seconds with the model already installed.
2. Start a replay evaluation using `local_only` with cloud consent disabled. Use the [credited replay procedure](firebase-worker.md#verify) to send current Core evidence. Open the record in the worker page and check the incident ID, evidence version and actual `on_device` source. The replay runner also checks the saved result and the absence of live delivery for the replay.
3. Run `prefer_local` with the browser model unavailable and cloud consent enabled on a test camera. Check the actual `in_cloud` result and repeat the consent check with the private camera. The fixture runner checks that Core sends zero cloud submissions for that camera.
4. Return to the local route. Freeze and resume the worker tab in the dedicated profile. Check that the lease expires during the freeze and a new evaluation returns `on_device` after resume. The [acceptance matrix](acceptance.md) lists the observed coverage and the remaining browser and App Check checks.

## Demonstration footage

| Time | Show on screen |
| --- | --- |
| 0:00–0:30 | A masked camera view and an incident in Companion. Explain that Core owns the capture, local detection and durable incident. |
| 0:30–1:15 | The same incident ID and evidence version in the Chrome worker. Show the local-only route, cloud consent disabled and the actual `on_device` result. Keep the overview visible while the response is fresh. |
| 1:15–1:55 | A second test camera with explicit consent. Show the actual `in_cloud` result with the local model unavailable and the protected camera's absence from the cloud submissions. |
| 1:55–2:40 | Close the worker while Core continues analyzing a live test feed. Show the Pose and Fire progress in Core and then return to the worker. |
| 2:40–2:55 | Show where to find the install instructions, runtime code and credits. |

Use English narration or English subtitles. Record the actual application and retain source and evidence credits. Crop secrets, device credentials and local private paths from the recording. The [official rules](https://firebasehybridaihackathon.devpost.com/rules) govern submission eligibility, access for judges, the public source repository and the demonstration requirements. The proposed platform is Web (Chrome) for the Hybrid Web worker. The treatment of the separate Rust Core and the rights to redistribute the bundled weights remain open in the [release matrix](../qualification/release-requirements.json).
