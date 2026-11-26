# StillWatch limitations

StillWatch is a supplemental monitor. Keep certified smoke and fire alarms, emergency procedures, and appropriate supervision in place.

- Local models can miss or flag fire, smoke, and falls late. Small flames, weak smoke, difficult lighting, occlusion, and interrupted camera feeds reduce the evidence available for detection.
- Mist, steam, reflections, and ordinary movements such as bending, sitting, or lying down can trigger Review alerts. The available evaluations support engineering comparisons. Independent estimates across new sites and people require more reviewed recordings.
- The shipped local policy raises Review on observed incidents. An Urgent fire alert requires the existing corroborated escalation policy. Treat a cloud assessment with insufficient evidence as a cue to inspect the original event.
- The optional Firebase check uses one opening packet per incident on cameras with cloud consent. A pending Review alert becomes available after the result or within 10 seconds of opening. With the cloud route already unavailable, Core delivers the local Review immediately. History records the local decision as it occurs. A clear view of a non-fire source behind an isolated detection or of a flame confined to a visible lighter or burner can change the pending Fire Review to Info. Ambiguous evidence keeps the local Review.
- Existing deployment checks cover local NAT/TURN and physical webcam capture. Compatibility with physical ONVIF cameras and traversal across independent internet networks depends on the installation. See the acceptance matrix for verified configurations.

See [Firebase configuration](docs/firebase-worker.md) and [release verification](IMPLEMENTATION_STATUS.md).
