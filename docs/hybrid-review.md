# StillWatch Next Hybrid Web walkthrough

StillWatch Next monitors the cameras attached to a Windows or Linux host. The Rust Core runs the local ONNX models and saves incidents with masked evidence. The Hybrid Web worker uses Firebase AI Logic to describe selected evidence. Operators can see the route reported by the Firebase SDK before deciding how to act on the description.

## Review environment

Use a dedicated review host or a disposable data directory, the Core payload and matching model pack from one build, and a supported Chrome profile with the local model installed. The reference walkthrough uses Windows x64, Chrome 157, Node 24 and Firebase JS SDK 13.3.0. Follow the [installation guide](operations.md) and the [Hybrid Web setup](firebase-worker.md#build-and-configure). Configure a dedicated Firebase test app with App Check and separate credentials for Core and Chrome. Use the credited replay from [baseline.json](../baseline.json).

1. Open the loopback worker page in the dedicated Chrome profile. Connect and wait for local model readiness. With the model installed, the recorded cold starts took about 156 seconds and 357 seconds. The longer run recovered after two warmup retries.
2. Start a replay evaluation using `local_only` with cloud consent disabled. Use the [credited replay procedure](firebase-worker.md#verify) to send current Core evidence. Open the record in the worker page and check the incident ID, evidence version and actual `on_device` source. The replay runner also checks the saved result and the absence of live delivery for the replay.
3. Run a cloud evaluation with consent enabled on a test camera. Check the actual `in_cloud` result and repeat the consent check with the private camera. The fixture runner checks that Core sends zero cloud submissions for that camera.
4. Return to the local route. Freeze and resume the worker tab in the dedicated profile. Then discard the tab and load it again from `chrome://discards`. In each case verify that the lease expires while the tab is unavailable and a new evaluation returns `on_device` after recovery. The recorded discard returned to ready in 4.22 seconds from reload.

## Opening review

A ready Firebase worker can assess the opening evidence before Fire Review is published. A clear view without fire or smoke can keep a brief local detection at Info. A flame visibly confined to a recognizable source can also stay at Info. Core uses the local decision when evidence is inconclusive, the worker is unavailable or the 15-second window expires.

The launch demonstration shows a local Gemini Nano result, a consented cloud result, a camera with zero cloud requests, and analysis during a worker outage. The device recording shows an injected Urgent used to test system notification delivery. [Footage sources](../baseline.json)
