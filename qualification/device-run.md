# Physical StillWatch device run

Attach this sheet to the candidate report for each device and network topology. Record the exact Core and Companion SHA-256, platform and OS version, device model, receiver role and allowed cameras, and any applicable signing or notification environment. Record a canary source ID and the expected detection window before the run. Preserve onset, Core opening, provider acceptance where available, OS presentation, audible alert, client receipt and operator ACK as separate observations.

| Phone state or transition | Verify on Android | Verify on iPhone |
| --- | --- | --- |
| Foreground, connected | Present a new canary, open the incident, verify receipt and independently ACK | Present a new canary, open the incident, verify receipt and independently ACK |
| Home, then a new incident | Observe the running foreground service and the new native alert | Observe the visible APNs alert and open its device-scoped incident |
| Locked and asleep | Observe notification and actual sound when enabled | Observe notification and actual sound when enabled |
| Ordinary process termination and relaunch | Observe service lifecycle and the alert index after relaunch | Record OS presentation and successful cold activation of the alert |
| System permission denied, then restored | Record the denied state and verify a later authorized canary | Record authorization status and renewed APNs registration |
| DND, Focus or silent mode | Record presentation and sound separately | Record presentation and sound separately |
| Force-stop or deliberate monitoring stop | Record which delivery routes remain available and the state after reopening | Record presentation and cold activation separately |
| Core disconnected, then recovered | Observe retained valid alert, reconnect and a new alert | Verify stale-handle resolution and a new alert after reconnect |
| Expiry, revoke or new attention cycle | Verify old action stops at the correct cycle | Verify expired and revoked handles cannot produce a receipt |
| Wi-Fi to mobile or VPN | Verify pinned control, refresh, revoke and recovery | Verify pinned control, refresh, revoke and recovery |

Use a test recipient and a controlled live video canary through the real pipeline. Identify native delivery fixtures separately and record the applicable supported and observed outcomes for each row. For external NAT use a phone on mobile data with local Wi-Fi disconnected. Record the observed TURN/TLS path, video decode, revocation and recovery. Record RTSP or RTSPS reconnect, authentication, stream geometry, masks and at least two viewers on each supported physical camera setup. Include Linux and Windows UVC and ONVIF with separate analysis and detail streams where available.
