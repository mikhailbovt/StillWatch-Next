# StillWatch Next operations

## Install the CPU Core

Install the Core on the machine that can reach the cameras. Use a local filesystem for SQLite and evidence. Keep the data directory writable by the service account and the installation directory writable by administrators only. Install the Companion separately on each operator's device.

The Windows x64 MSI installs the StillWatchCore service under LocalService. Service settings are in the StillWatch subdirectory of ProgramData. Back up that directory before changing versions. On Linux, install the DEB on Ubuntu 24.04 x64 or the RPM on Fedora 44 x64, review the environment in /etc/stillwatch/core.conf, then enable the service with the command below. Allow USB access through the specific device's operating system permissions if needed.

The Windows Core installer includes the locked Microsoft Visual C++ x64 runtime beside the executables. The SCM restarts the Core after up to two unexpected exits, with a three-second delay. Review the System event log if restarts are exhausted. The failure count resets after a day without an unexpected exit.

~~~sh
sudo systemctl enable --now stillwatch.service
sudo -u stillwatch stillwatch-cli doctor --data-dir /var/lib/stillwatch
~~~

The Core can start without models and report its capture and storage state. To enable analysis, provision a verified model pack and configure its manifest digest. In the Linux service environment, use STILLWATCH_MODEL_DIR and STILLWATCH_MODEL_SHA256. In Windows core.json, set model_pack, model_manifest_sha256, ort_library, and ort_library_sha256. The installed manifest provides the expected ORT digest. An administrator provides the trusted model digest separately.

The packaged worker's capture-doctor command reports whether the loaded libav build can confine RTSP connections. Its doctor command also checks that the provided ORT runtime can open all three pinned ONNX graphs.

~~~text
stillwatch-worker capture-doctor
stillwatch-worker doctor ORT_LIBRARY ORT_SHA256 MODEL_PACK MANIFEST_SHA256
~~~

## Replace a model pack

Stage the complete new pack in a separate versioned directory owned by the administrator. Preserve the previous pack. The worker checks the class map, hashes, ONNX operators, opset, and tensor declarations before opening sessions. The current profile uses embedded weights.

For atomic selection, set `model_selection_file` in core.json to an absolute file path in a protected directory. On Linux, use `STILLWATCH_SETTINGS_FILE` to load that core.json and remove model and CUDA assignments from the service environment. The service account needs read access to the selection, pack, and runtime. Run the command below from the installed worker directory with the trusted digests. For a CUDA deployment, append `--cuda DEVICE CUDA_PROVIDER_SHA256 ORT_SHARED_PROVIDER_SHA256` and perform activation on the selected GPU host.

~~~text
stillwatch-worker activate-model-pack SELECTION_FILE ORT_LIBRARY ORT_SHA256 STAGED_PACK MANIFEST_SHA256
~~~

The command compares all three models with the reference fixtures on the selected execution provider and atomically replaces the selection file after the checks pass. A running service keeps its current sessions. Use the following command with the service's OS account, runtime paths and worker settings to check the activation without binding listeners or opening the data directory. On a dedicated GPU, allow a maintenance window if the previous workers occupy the GPU's available memory.

~~~text
stillwatch-core --check-inference --data-dir DATA --settings-file SETTINGS_JSON
~~~

Restart Core during the maintenance window and confirm fresh results on active cameras. Newly created incidents record the manifest digest and local policy version. To roll back, run the activation command with the previous pack and runtime, then repeat the readiness check and restart.

## Windows package build and upgrade

The Windows Core build uses WiX 5.0.2 and its `WixToolset.Util.wixext/5.0.2` extension. Use `wix extension add -g WixToolset.Util.wixext/5.0.2` on the packaging machine. The minimal Windows libav build uses the Win32-thread variant of the x64 MinGW compiler.

Windows installers take the three-component product version from the staged manifest. For major upgrades, install a strictly higher version with the same product family after taking an offline backup. Keep the prior installer and compatible data together for rollback. If rolling back, stop and remove the newer product, restore the compatible backup with its original access permissions, and install the earlier version. Verify that `LocalService` can access `ProgramData\StillWatch` and that permissions still exclude other interactive users. A file copy that drops the service ACL can cause Windows Installer error 1920 during startup.

## Enroll a device

Set lan_listen to a specific reachable private IP and port in core.json. Enable lan_advertise if clients on the same network should discover the Core. On Linux set STILLWATCH_SETTINGS_FILE to the absolute path of core.json. A reachable VPN address can also be used for pairing. Keep the maintenance endpoint on loopback.

~~~sh
sudo -u stillwatch stillwatch-cli pair --data-dir /var/lib/stillwatch --role owner
~~~

Scan the five-minute QR code in the Companion or use --json for manual pairing. Keep the offer private and transfer it through a trusted channel. The owner can enroll scoped operator and viewer devices and revoke old devices from Settings. Re-pair devices after a restore.

When a Core's address or certificate changes, verify the change with the operator and compare the new fingerprint using a trusted channel. In the Companion, open the old Core's Settings, choose **Forget Core on this device**, and enroll with the fresh offer. The Core owner can revoke the superseded device registration after enrollment succeeds.

## Cameras and video

Add the numeric control address of each authorized network camera to the rtsp_peers array in core.json. Enter the same address and port in the Companion. For authenticated cameras, place a private JSON file containing username and password in the configured camera_credentials_dir. Name the file with its UUID and the .json suffix. Use only the UUID when configuring the camera. The RTSP adapter rejects redirects and confines negotiated UDP media to the control peer. RTSPS requires a trusted certificate with an IP subject alternative name matching the configured camera. Windows SChannel verifies the chain and its revocation status in the worker account. The initial probe allows extra time for this check. The camera setup timeout in the SDKs covers it.

For ONVIF, add each approved numeric device-service URL to `onvif_endpoints` in core.json. Set `onvif_interface` to the local IPv4 address of the Core's camera network to enable multicast discovery. The owner can open **Find ONVIF stream** in the camera editor, discover devices on that network, or enter an approved device URL directly. Set the credential reference in the camera editor before opening the picker for a camera that requires authentication. The picker reads video profiles through Media or Media2. Select a profile, allow its RTSP socket in `rtsp_peers`, and probe it before enabling monitoring. ONVIF calls use the same numeric address, scheme and port as the approved device URL. For HTTPS, configure `onvif_ca_file` with the PEM trust anchors for those cameras and use certificates with the matching IP subject alternative names. Keep legacy HTTP cameras on a private camera network. [ONVIF interface specifications](https://www.onvif.org/profiles/specifications/)

For cameras attached to the host, inspect stable device IDs with stillwatch-cli cameras --data-dir DATA devices. To add a source through the CLI, prepare a CameraConfig JSON file and pass a unique operation key. Use the same key and same JSON when retrying an interrupted request.

~~~text
stillwatch-cli cameras --data-dir DATA add --config CAMERA_JSON --key OPERATION_ID
stillwatch-cli cameras --data-dir DATA list
~~~

For replay, place a supported video in the Core sources directory. Enable pace for source-clock playback and repeat for repeated playback. A repeat creates a fresh source epoch. Replay incidents cannot initiate real alert delivery.

The owner can also select **Upload replay video** in the Companion. The Core probes a decoded frame before publishing the uploaded file. Each upload may contain up to 128 MiB and the managed upload directory holds up to 2 GiB. The original recording is stored on the Core host. Camera masks apply when replay is decoded and shared. Include required source videos in the host's backup and remove unused uploads from the replay directory after removing their camera configurations.

Save and enable the camera before marking a controlled flame. Open **Camera settings**, choose **Add fire region**, and tap the corners in order around the expected flame. Choose the source and expected flame mode, then apply and save. After changing the source or privacy masks, save those changes and review the regions on a fresh masked snapshot. Core requires another review when it detects a change in the camera geometry.

Set mediamtx, mediamtx_sha256, media_encoder, media_encoder_sha256 and media_ice_udp in core.json to enable video. Use the installed MediaMTX and FFmpeg/OpenH264 binaries and their digests from manifest.json. On Linux you can set STILLWATCH_ICE_UDP to enable video with the installed binaries. Bind ICE to the Core's private address. Verify decoded video separately from control connectivity.

When playback stalls, the Companion can show a masked snapshot with the age of the frame. It continues fetching snapshots while the camera and control route are reachable. Use **Reconnect video** after the media route recovers.

## Migration, backup and restore

Stop the legacy writer and take an offline backup of its data directory. Provide the first camera mapping as a JSON object with a camera field containing a CameraConfig. Set same_geometry to true only after verifying the camera framing and normalized privacy masks. The imported camera starts disabled. Review its source and masks before enabling it.

~~~text
stillwatch-cli migrate --legacy-data LEGACY_COPY --destination NEW_DATA --config CAMERA_MAPPING --dry-run
stillwatch-cli migrate --legacy-data LEGACY_COPY --destination NEW_DATA --config CAMERA_MAPPING
stillwatch-cli backup-offline --data-dir NEW_DATA --destination BACKUP
stillwatch-cli restore --backup BACKUP --destination RESTORED_DATA
~~~

Compare the source snapshot digest and row counts from both migration runs. The migration manifest also records how many historical images are available and the digest of the new database. Archived assessments remain associated with their original provider. Previously open incidents require reassessment and the migration creates no notification intents. For cutover, use a separate directory for the new Core and keep the old backup for rollback. Restore data compatible with the previous binary when rolling back a schema change.

Core retains evidence JPEGs for 24 hours by default. Set evidence_retention_hours before cutover to choose how long copied images should stay accessible. Retention applies to the original capture time. Expired evidence keeps its historical metadata and is shown as unavailable.

Resolved incidents and their related metadata age out after 30 days by default. Use `history_retention_days` to set the window before starting Core. Incident metadata stays available for at least the seven-day command retry window. Open incidents and any unexpired evidence remain accessible. The window also governs older configuration revisions and trust audit records. After older events are pruned, a Companion with an expired cursor refreshes its snapshot. Back up the Core before reducing retention.

## Operator checks

- Run `stillwatch-cli doctor --data-dir DATA --output diagnostics.json` on the Core host to save readiness, inference worker and backend state, media router state, camera runtime, and semantic provider status. Camera entries use report-local numbers. The file excludes host addresses, instance and camera IDs, camera names, credentials, and images. Treat unavailable analysis as unknown. If the worker reports a model pack or runtime startup error, correct the configured path, digest, or execution provider and restart Core.
- In a camera viewer, check both the timestamp and the decoded picture. Loss of a private VPN or UDP path can affect video separately from the control API.
- Run stillwatch-alarm-agent monitor from a second authorized host to check the Core independently. It emits changes in control-path health. Check media separately.
- Use the platform's notification settings to verify Companion permissions. The Companion uses foreground polling in this profile. Test the operating system's sound route and interruption behavior for each actual device.
- The documented short CPU replay runs cover the tested machine and workload. Use measured results for sizing. Event recall and false-alert rates require independently labeled scenes.

For Firebase configuration, see firebase-worker.md. For private VPN, outbound relay and Coturn configuration, see remote.md. For process adapter integration, see worker-protocol.md.

## Desktop alarm agent

The Companion installer includes the native agent. Pair it as a separate viewer with the cameras you want to monitor. Use a fresh viewer offer JSON and remove the temporary copy after enrollment. The agent accepts the offer on standard input. A successful notification receipt records that the agent handled an alert. The operator acknowledges the incident in Companion.

Run the following in the Linux desktop session to test sound and opt in at sign-in. The packaged user unit follows `graphical-session.target`. A graphical environment must activate that target at login. You can start the unit for the current session and inspect its status with `systemctl --user status stillwatch-alarm-agent`. [systemd graphical session target](https://www.freedesktop.org/software/systemd/man/255/systemd.special.html)

~~~sh
state_dir=${XDG_STATE_HOME:-$HOME/.local/state}/stillwatch
install -d -m 0700 "$state_dir"
stillwatch-alarm-agent enroll --state "$state_dir/alarm-agent.json" --label 'Desk alarm' < offer.json
stillwatch-alarm-agent test-sound
systemctl --user daemon-reload
systemctl --user enable --now stillwatch-alarm-agent.service
~~~

On Windows, use PowerShell in the intended user's interactive session. The task starts at that user's sign-in, runs with limited privileges and remains visible in Task Scheduler. DPAPI protects the enrolled credentials for that user. [Task Scheduler interactive principal](https://learn.microsoft.com/en-us/powershell/module/scheduledtasks/new-scheduledtaskprincipal)

~~~powershell
$agent = Join-Path $env:ProgramFiles 'StillWatch Companion\stillwatch-alarm-agent.exe'
$state = Join-Path $env:LOCALAPPDATA 'StillWatch\alarm-agent.json'
Get-Content -Raw -LiteralPath .\offer.json | & $agent enroll --state $state --label 'Desk alarm'
if ($LASTEXITCODE -ne 0) { throw 'Alarm agent enrollment failed' }
& $agent test-sound
if ($LASTEXITCODE -ne 0) { throw 'The desktop audio route is unavailable' }
$user = [Security.Principal.WindowsIdentity]::GetCurrent()
$task = "StillWatch alarm agent ($($user.User.Value))"
$principal = New-ScheduledTaskPrincipal -UserId $user.Name -LogonType Interactive -RunLevel Limited
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $user.Name
$settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew `
    -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
$action = New-ScheduledTaskAction -Execute $agent -Argument ('run --state "{0}" --audible' -f $state)
Register-ScheduledTask -TaskName $task -Action $action -Principal $principal -Trigger $trigger -Settings $settings
Start-ScheduledTask -TaskName $task
~~~

Closing the Companion window leaves the agent available in the signed-in session. Use `systemctl --user stop stillwatch-alarm-agent` on Linux or **End** on the named Windows task to stop the current run. Use `systemctl --user disable --now stillwatch-alarm-agent` or **End** followed by **Disable** in Task Scheduler to turn off automatic start. Use the foreground notification switch in Companion to control the app's own system notifications. The Core owner can revoke the agent's pairing in Settings.

## Build mobile Companion

Use the Flutter framework revision in the CI workflow and run `flutter pub get --enforce-lockfile` from the Companion directory before building. This generates the native Android registrant for secure storage, file selection, notifications, discovery, and WebRTC. For Android-only builds on Windows, you can use Flutter's `config --no-enable-windows-desktop` in a separate Flutter configuration directory. Windows desktop builds still require native symbolic link support.

Use `flutter build apk --release --target-platform android-arm64` and `flutter build appbundle --release --target-platform android-arm64`. The release build refreshes Android's plugin registrant to exclude the development-only integration test plugin. Android release signing reads the local, ignored key.properties file. For an iOS archive, use the locked Flutter version on a macOS host with Xcode and a matching Apple distribution or development profile. Record the signing profile type and applicable devices when handing over an iOS build.

## CUDA qualification

The CUDA backend uses the same locked FP32 model tensors and policies. Pass cuda_device, cuda_provider_sha256 and cuda_shared_sha256 on the Core command line with the existing model and ORT flags. Use dashes in flag names. In core.json use a cuda object with device, provider_sha256 and shared_sha256. Keep the provider libraries beside the matching ONNX Runtime library and install the corresponding NVIDIA user-space runtime through an authorized source. An invalid or missing provider prevents the requested backend from starting.

Use the CPU profile for qualified local operation. The CUDA Compose override mounts operator-provisioned CUDA, cuDNN and ONNX Runtime libraries read-only. It uses the NVIDIA Container Toolkit's CDI device mapping with Docker Engine 28.3 or later. Docker reads the generated CDI specification from its configured directories. The WSL profile uses `nvidia.com/gpu=all`. On Linux, set `STILLWATCH_GPU_DEVICE` to the desired device name from `nvidia-ctk cdi list` when the container must use a specific GPU. Run the worker's model verification with `--cuda DEVICE PROVIDER_SHA256 SHARED_SHA256` before relying on this backend. Tensor or observation drift outside the existing bounds leaves the profile experimental. [NVIDIA CDI support](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/cdi-support.html), [Docker CDI devices](https://docs.docker.com/reference/cli/docker/container/run/#cdi-devices)

Build the CUDA profile from the local verified CPU image and use both Compose files. The CUDA image configures its writable kernel cache under the existing `/tmp` tmpfs. The worker warms one tensor geometry before reporting readiness, within a bounded startup deadline. The live inference deadline stays two seconds.

~~~sh
docker build --pull=false -f deploy/packaging/Dockerfile.cuda -t stillwatch-core:0.1.0-cuda .
docker compose -f deploy/packaging/compose.cpu.yaml -f deploy/packaging/compose.cuda.yaml up -d
~~~
