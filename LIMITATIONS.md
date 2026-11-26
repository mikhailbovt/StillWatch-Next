# StillWatch limitations

StillWatch is a supplemental monitor. Keep certified smoke and fire alarms, emergency procedures, and appropriate supervision in place.

- Local models can miss or flag fire, smoke, and falls late. Small flames, weak smoke, difficult lighting, occlusion, and interrupted camera feeds reduce the evidence available for detection.
- Mist, steam, reflections, and ordinary movements such as bending, sitting, or lying down can trigger Review alerts. The available evaluations support engineering comparisons. Independent estimates across new sites and people require more reviewed recordings.
- The shipped local policy raises Review on observed incidents. An Urgent fire alert requires the existing corroborated escalation policy. Treat a cloud assessment with insufficient evidence as a cue to inspect the original event.
- The optional Firebase check uses one opening packet per incident on cameras with cloud consent. Core waits up to 15 seconds from the observation and applies an earlier answer immediately. Clear evidence of an ordinary scene or a confined flame can keep a brief local smoke detection at Info. Uncertain, missing or late evidence releases the local Review. When the cloud route is already unavailable, the local decision is immediate.
- In limited visibility, two weak smoke detections without flame or growth need to span at least one second. A third observation can confirm the cue sooner. Brief smoke that disappears in poor lighting can be missed.
- Existing deployment checks cover local NAT/TURN and physical webcam capture. Compatibility with physical ONVIF cameras and traversal across independent internet networks depends on the installation. See the acceptance matrix for verified configurations.

See [Firebase configuration](docs/firebase-worker.md) and [the Core release](https://github.com/mikhailbovt/StillWatch-Next/releases/tag/v1.0.1).
