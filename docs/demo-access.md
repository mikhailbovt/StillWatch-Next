# Demo access

StillWatch Companion pairs with a StillWatch Core. The TestFlight link installs the iOS beta. Each tester also needs a reachable demo Core and their own enrollment offer.

## Run a local walkthrough

1. Build or install Core and verify its model configuration with [the operations guide](operations.md). Use a separate test data directory and a test camera or credited replay.
2. Start Core with the LAN profile reachable from the test device. Keep the administrative HTTP endpoint on loopback. Use [the LAN setup](operations.md#enroll-a-device) or [the native outbound relay](remote.md#outbound-control-relay).
3. Create a scoped viewer offer on the Core host for the camera being demonstrated.

   ```sh
   stillwatch-cli pair --data-dir DEMO_DATA --role viewer --camera DEMO_CAMERA_ID --scope streams-read
   ```

4. In Companion select **Add a Core**, scan the QR code, verify the Core certificate fingerprint and pair. An offer expires in five minutes and accepts a single enrollment. The enrolled device has a separate, revocable session.
5. Open **Cameras** and the camera's live view. Use [the Hybrid Web walkthrough](hybrid-review.md) to reproduce the Firebase AI Logic route independently. Use a scoped operator identity on the disposable Core to demonstrate acknowledgement.

## Prepare the external beta

Add the selected build to a TestFlight external group for Beta App Review. Provide Apple with the beta description, review contact, feedback address and working access instructions. After acceptance, the external group can issue an installation link with a tester limit. [Apple TestFlight setup](https://developer.apple.com/help/app-store-connect/test-a-beta-version/invite-external-testers) · [Beta review information](https://developer.apple.com/help/app-store-connect/test-a-beta-version/provide-test-information)

Use a dedicated Core with a small set of credited or synthetic feeds, finite storage and a public TLS route, separate from personal cameras and the production Core. Serve the tester's own short-lived viewer offer through a bounded enrollment service or issue it privately on request. Keep that service's issuance credentials on the demo host. Limit enrollment and concurrent video viewers. Grant only the test camera and streaming scope. Revoke demo devices at the end of testing.

Before sharing a remote Core offer, test the demo endpoint from a second network. Verify enrollment and snapshot access, then negotiate a WHEP stream. Remote WebRTC can require separately configured ICE and TURN. Verify the phone's background behavior and notification permissions on the actual beta build. [Remote access and TURN](remote.md)
