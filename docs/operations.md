# StillWatch Next operations

## Install the CPU Core

Install the Core on the machine that can reach the cameras. Use a local filesystem for SQLite and evidence. Keep the data directory writable by the service account and the installation directory writable by administrators only. Install the Companion separately on each operator's device.

The Windows x64 MSI installs the StillWatchCore service under LocalService. Service settings are in the StillWatch subdirectory of ProgramData. Back up that directory before changing versions. On Linux, install the DEB on Ubuntu 24.04 x64 or the RPM on Fedora 44 x64, review the environment in /etc/stillwatch/core.conf, then enable the service with the command below. Allow USB access through the specific device's operating system permissions if needed.

The Linux package version comes from the three-part version in the staged manifest. See [upgrade and rollback](#upgrade-or-roll-back-linux-core) for the service and data sequence.

The Windows Core installer includes the locked Microsoft Visual C++ x64 runtime beside the executables. The SCM restarts the Core after up to two unexpected exits, with a three-second delay. Review the System event log if restarts are exhausted. The failure count resets after a day without an unexpected exit.

The CPU and CUDA Compose profiles use a 512 MiB temporary filesystem and rotate the Core log across three 16 MiB files. The Linux package writes to the systemd journal. Set the journal's storage budget and retention through the host's journald policy. [Docker local logging driver](https://docs.docker.com/engine/logging/drivers/local/), [systemd journal limits](https://www.freedesktop.org/software/systemd/man/latest/journald.conf.html)

~~~sh
sudo systemctl enable --now stillwatch.service
sudo -u stillwatch stillwatch-cli doctor --data-dir /var/lib/stillwatch
~~~

The Core can start without models and report its capture and storage state. To enable analysis, provision a verified model pack and configure its manifest digest. In the Linux service environment, use STILLWATCH_MODEL_DIR and STILLWATCH_MODEL_SHA256. In Windows core.json, set model_pack, model_manifest_sha256, ort_library, and ort_library_sha256. The installed manifest provides the expected ORT digest. An administrator provides the trusted model digest separately.

## Analysis and viewing capacity

On Linux, `stillwatch-core --execution-host` reports usable RAM in complete MiB, expressed as bytes. Freeze the hardware profile from this command on the target. Its capacity excludes partial MiB that can change with kernel page accounting.

The camera probe reports raw capture memory and measured source rate. Its analysis capacity estimate is `unknown` until Core loads a matching qualified execution profile. The owner status reports the active profile and allowance. A verified profile reserves compute when an enabled camera is committed. A probe can include the camera ID when replacing a reservation and the intended Pose, Fire and evidence sampling. Restart reapplies the profile to enabled cameras.

Use `stillwatch-core --data-dir DATA --execution-host` to collect the current fingerprint. Supply the same model settings to include the configured CUDA device. If qualification used a specific Tokio worker count, set `core_runtime_threads` in the protected Core settings file or pass `--core-runtime-threads` for a foreground launch. Use the same setting for the host probe. The capture worker's `capture-doctor` reports the decoder identity. To activate a verified execution profile, set `execution_profile` in the protected Core settings file with `profile_file`, `profile_sha256`, `report_file`, and `report_sha256`. Use absolute paths and SHA-256 hex. The profile digest must be the validator report's `execution_profile_digest`. Core checks the report, fingerprint, Tokio worker count, runtime pins, role hashes, tensor shape and operating settings at startup. A mismatch reports unknown capacity and preserves locally configured operation.

Set `inference_workers` and `inference_threads` in the protected Core settings file for service deployments. Foreground flags `--inference-workers` and `--inference-threads` override these settings. The defaults are one worker and four ONNX Runtime threads per worker.

CUDA Graph capture is opt-in through `cuda.graph_shape` in Core settings or `--cuda-graph-shape HEIGHT WIDTH` alongside the pinned CUDA libraries on the command line. Use one shape from rectangular ModelPack v3 with one or two inference workers. Each worker loads and warms its own CUDA sessions. The execution profile must cover the chosen worker count and its measured memory. Core reports degraded analysis for a camera whose frames require a different tensor shape. Its capture and streaming remain available. A qualification report for the same CUDA graph shape is required to claim verified capacity.

Without a matching profile, optional `media_workload_limits` in Core settings controls preview and detail publishers, additional detail decoder permits, total weighted publisher units, concurrent viewers, concurrent JPEG encoders and the trailing-minute JPEG request budget. A matching profile sets these allowances. Viewers share the publisher for the same camera scope and stream profile. Closing the last viewer releases resources after the publisher stops. Additional viewers or JPEG work can receive HTTP 429 while local analysis continues.

Each snapshot admitted for encoding occupies one slot for 60 elapsed seconds. Linux measures that interval with the raw monotonic clock and Windows uses QueryPerformanceCounter. A system time correction leaves the JPEG allowance stable. [Linux clocks](https://man7.org/linux/man-pages/man2/clock_gettime.2.html), [Rust platform clocks](https://doc.rust-lang.org/std/time/struct.Instant.html#underlying-system-calls)

The packaged worker's capture-doctor command reports whether the loaded libav build can confine RTSP connections. Its doctor command also checks that the provided ORT runtime can open all three pinned ONNX graphs.

~~~text
stillwatch-worker capture-doctor
stillwatch-worker doctor ORT_LIBRARY ORT_SHA256 MODEL_PACK MANIFEST_SHA256
~~~

## Replace a model pack

Stage the complete new pack in a separate versioned directory owned by the administrator. Preserve the previous pack. The worker checks the class map, hashes, ONNX operators, opset, and tensor declarations before opening sessions. The current profile uses embedded weights.

Manifest v3 also declares the source revision, weight hashes, active roles, permitted tensor shapes, observation specification digest and compatible policy. The worker warms the declared shapes and verifies the selection before starting analysis. Manifest v2 remains supported for existing installations. The model exporter can write v3 with `--versioned-contract`.

To add a missing rectangular tensor shape, reuse reference cases from a pinned manifest for the same weights and observation contract. The extension keeps the base pack's policy and verifies every copied fixture. Verify the resulting pack with the native worker before packaging or activating it.

~~~sh
python tools/model-export/extend_shapes.py --base BASE_PACK --base-sha256 BASE_SHA256 --reference REFERENCE_PACK --reference-sha256 REFERENCE_SHA256 --output NEW_PACK
stillwatch-worker verify-model-pack ORT_LIBRARY ORT_SHA256 NEW_PACK NEW_PACK_SHA256
~~~

For atomic selection, set `model_selection_file` in core.json to an absolute file path in a protected directory. On Linux, use `STILLWATCH_SETTINGS_FILE` to load that core.json and remove model and CUDA assignments from the service environment. The service account needs read access to the selection, pack, and runtime. Run the command below from the installed worker directory with the trusted digests. For a CUDA deployment, append `--cuda DEVICE CUDA_PROVIDER_SHA256 ORT_SHARED_PROVIDER_SHA256` and perform activation on the selected GPU host.

~~~text
stillwatch-worker activate-model-pack SELECTION_FILE ORT_LIBRARY ORT_SHA256 STAGED_PACK MANIFEST_SHA256
~~~

Match the PoseBalanced tensor to the source aspect ratio and check the active model manifest before changing a source. The fixed CUDA Graph profile uses the declared shape.

The command compares all three models with the reference fixtures on the selected execution provider and atomically replaces the selection file after the checks pass. A running service keeps its current sessions. Use the following command with the service's OS account, runtime paths and worker settings to check the activation without binding listeners or opening the data directory. On a dedicated GPU, allow a maintenance window if the previous workers occupy the GPU's available memory.

~~~text
stillwatch-core --check-inference --data-dir DATA --settings-file SETTINGS_JSON
~~~

Restart Core during the maintenance window and confirm fresh results on active cameras. Newly created incidents record the manifest digest and local policy version. To roll back, run the activation command with the previous pack and runtime, then repeat the readiness check and restart.

## Windows package build and upgrade

The `blobs` subdirectory of the Core data directory can be an administrator-mounted folder for a dedicated local volume. Configure the volume's permissions for the service identity before starting Core. The data directory still holds SQLite, the instance lock and local secrets. Core recognizes Windows volume mounts through the volume manager, and its backup reads the mounted JPEG files through the same data directory layout.

The Windows Core build uses WiX 5.0.2 and its `WixToolset.Util.wixext/5.0.2` extension. Use `wix extension add -g WixToolset.Util.wixext/5.0.2` on the packaging machine. The minimal Windows libav build uses the Win32-thread variant of the x64 MinGW compiler.

For a signed release, run the packager with the Windows SDK SignTool, the release certificate thumbprint and the RFC 3161 timestamp URL used by the release signer. The packager uses the current user's Personal certificate store. Add `-MachineSigningStore` when the release identity belongs to the machine store.

~~~powershell
$sign = @{SignTool = $signTool; SigningThumbprint = $thumbprint; TimestampUrl = $timestampUrl}
./deploy/packaging/windows.ps1 -Payload $payload -Wix $wix -Output $installer @sign
~~~

Use `-Companion` and a `.exe` output to build the Burn installer. The signing step verifies the embedded MSI, the detached Burn engine and the complete bundle before publishing the bundle to the requested path. To publish a standalone Companion MSI, use a separate `.msi` output. Record and use the final signed artifact's SHA-256 for the installation tests. [Microsoft SignTool](https://learn.microsoft.com/en-us/windows/desktop/seccrypto/signtool), [WiX bundle signing](https://docs.firegiant.com/wix/tools/signing/)

The Core MSI declares Mikhail Bovt as its publisher in Windows installer metadata. SignPath Foundation signing is planned for public distribution. Submit the open-source project after it has an eligible public release in the package form requested for signing. Check the distribution rights for the files in that package. The signing certificate is issued to SignPath Foundation. [SignPath Foundation eligibility and signing policy](https://signpath.org/terms)

Windows installers take the three-component product version from the staged manifest. Use a higher version in the same product family for a major upgrade. [Windows upgrade and rollback](#upgrade-or-roll-back-windows-core)

## Distribution inventory

Keep the SHA-256 manifest and SBOM for each exact installer or container. Core payload manifests list the digest of every file. The model lock pins the source checkpoints, export configuration and ONNX files. The native lock pins the upstream archives, binaries and local FFmpeg patch. [Model origins](../models/THIRD_PARTY_NOTICES.md) · [Native dependency lock](../models/native-deps.lock.json)

| Component | Release evidence |
| --- | --- |
| StillWatch Next Core and Companion | Package the application license and baseline notice. Companion includes both through Flutter's generated notices archive. Keep the dependency SBOM tied to the build lockfiles and retain notices for native plugins. [Project notice](../NOTICE) · [Companion dependency lock](../apps/companion/pubspec.lock) |
| Pose, PoseBalanced and Fire | Include the AGPL-3.0 text and model notices. Retain the checkpoint provenance and export recipe for the supplied ONNX files. Record the rights decision for the exact model delivery before distribution. The Fire publisher identifies D-Fire as training data, and its source-image lineage is still open. [Model origins](../models/THIRD_PARTY_NOTICES.md) · [Ultralytics terms](https://www.ultralytics.com/license) |
| FFmpeg 8.1.3 capture and encoder | Preserve the source archive, capture patch, both configure records and both build scripts from the payload. Capture links shared libav libraries. The encoder uses the pinned OpenH264 2.7.0 source and includes its license and archive. [FFmpeg distribution checklist](https://ffmpeg.org/legal.html) · [OpenH264 license](https://github.com/cisco/openh264/blob/v2.7.0/LICENSE) |
| ONNX Runtime 1.31.0 and MediaMTX 1.21.3 | Retain the MIT licenses and ONNX Runtime third-party notices. The native lock identifies each target build and the ORT CUDA provider libraries. The documented CUDA Compose profile mounts CUDA libraries and the model pack provisioned by the operator. [CUDA profile](../deploy/packaging/compose.cuda.yaml) |
| Visual C++ 14.51.36231.0 | The `1802bad` Windows payload contains five CRT DLLs. Their SHA-256 digests match the corresponding files in the Visual Studio Build Tools 2026 redistributable directory. All five files have valid Microsoft Authenticode signatures. The release decision depends on the applicable Visual Studio license terms. [Microsoft redistribution terms](https://learn.microsoft.com/en-us/visualstudio/releases/2026/redistribution#distributable-code-files-for-visual-studio-2026-and-later) |
| Evaluation footage and demo assets | Retain attribution, permissions and lineage for the clips used. Use the corpus records when selecting material for a public demo. [Development corpora](../models/evaluations/development-corpora.json) · [Baseline footage credits](../baseline.json) |

The Core DEB installs copyright information at `/usr/share/doc/stillwatch-core/copyright`. Source archives, notices and build recipes are in `/usr/libexec/stillwatch/licenses` in both Linux formats. The RPM license tag includes AGPL-3.0-only when the payload includes weights that declare the license. The release decision for G4 is recorded against the exact artifacts in the [acceptance matrix](acceptance.md).

## Enroll a device

Set lan_listen to a specific reachable private IP and port in core.json. Enable lan_advertise if clients on the same network should discover the Core. On Linux set STILLWATCH_SETTINGS_FILE to the absolute path of core.json. A reachable VPN address can also be used for pairing. Keep the maintenance endpoint on loopback.

~~~sh
sudo -u stillwatch stillwatch-cli pair --data-dir /var/lib/stillwatch --role owner
~~~

Scan the five-minute QR code in the Companion or use --json for manual pairing. Keep the offer private and transfer it through a trusted channel. The owner can enroll scoped operator and viewer devices and revoke old devices from Settings. Re-pair devices after a restore.

When a Core's address or certificate changes, verify the change with the operator and compare the new fingerprint using a trusted channel. In the Companion, open the old Core's Settings, choose **Forget Core on this device**, and enroll with the fresh offer. The Core owner can revoke the superseded device registration after enrollment succeeds.

## Cameras and video

When adding or enabling a camera in Companion, review the decoded codec, resolution, clock quality, available camera slots and raw frame memory before saving. The estimate for an edit releases the existing reservation for that camera. Core checks both limits again in the write transaction because other camera settings can change before confirmation. Inference throughput still depends on the active sources and host. Check the measured cadence after activation.

Check the analysis preview with a person standing in view. Their feet should point toward the bottom of the picture. The fall detector measures posture relative to the decoded frame. A camera mounted sideways can produce a persistent low-posture Review for a standing person. Set the image rotation on the camera or in the upstream encoder, then verify both streams and redraw privacy masks and controlled zones if their positions changed.

Add the numeric control address of each authorized network camera to the rtsp_peers array in core.json. Enter the same address and port in the Companion. For authenticated cameras, provision credentials on the Core host and enter their UUID in the camera configuration. The RTSP adapter rejects redirects and confines negotiated UDP media to the control peer. RTSPS requires a trusted certificate with an IP subject alternative name matching the configured camera. Windows SChannel verifies the chain and its revocation status in the worker account. The initial probe allows extra time for this check. The camera setup timeout in the SDKs covers it.

On Windows, provision new camera credentials as machine-scope DPAPI blobs. Core reads `UUID.dpapi` before the corresponding JSON file. Run the following in an elevated Windows PowerShell on the Core host after installing the MSI. The credential directory inherits the private ACL of the installed data directory. Set `camera_credentials_dir` to that directory. A custom directory also requires an ACL that limits access to Administrators, SYSTEM and the Core service account. Machine-scope DPAPI binds the blob to this Windows installation, and any account that can read the blob on that host can decrypt it. Use a fresh reference for rotation or replace an existing blob atomically, then probe the camera and remove its old plaintext file. Reprovision protected credentials when restoring to a different machine. [Windows Data Protection API](https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata)

~~~powershell
Add-Type -AssemblyName System.Security
$directory = Join-Path $env:ProgramData 'StillWatch\camera-credentials'
New-Item -ItemType Directory -Path $directory -Force | Out-Null
$id = [Guid]::NewGuid()
$camera = Get-Credential -Message 'Camera username and password'
$json = @{username=$camera.UserName; password=$camera.GetNetworkCredential().Password} | ConvertTo-Json -Compress
$plain = [Text.Encoding]::UTF8.GetBytes($json)
try {
    $blob = [Security.Cryptography.ProtectedData]::Protect(
        $plain, $null, [Security.Cryptography.DataProtectionScope]::LocalMachine)
    [IO.File]::WriteAllBytes((Join-Path $directory "$id.dpapi"), $blob)
    $id
} finally {
    [Array]::Clear($plain, 0, $plain.Length)
    Remove-Variable json,plain,camera
}
~~~

On Linux and NAS, mount the credential directory from the host's private secret store, or supply a protected directory with files owned by the service account and mode 0600. Use `UUID.json` with `username` and `password` fields. Limit the service's read access to the credential files it needs. Back up the secrets separately under the host's protection.

For ONVIF, add each approved numeric device-service URL to `onvif_endpoints` in core.json. Set `onvif_interface` to the local IPv4 address of the Core's camera network to enable multicast discovery. The owner can open **Find ONVIF stream** in the camera editor, discover devices on that network, or enter an approved device URL directly. Set the credential reference in the camera editor before opening the picker for a camera that requires authentication. The picker reads video profiles through Media or Media2. Select a profile, allow its RTSP socket in `rtsp_peers`, and probe it before enabling monitoring. ONVIF calls use the same numeric address, scheme and port as the approved device URL. For HTTPS, configure `onvif_ca_file` with the PEM trust anchors for those cameras and use certificates with the matching IP subject alternative names. Keep legacy HTTP cameras on a private camera network. [ONVIF interface specifications](https://www.onvif.org/profiles/specifications/)

For cameras attached to the host, inspect stable device IDs with stillwatch-cli cameras --data-dir DATA devices. To add a source through the CLI, prepare a CameraConfig JSON file and pass a unique operation key. Use the same key and same JSON when retrying an interrupted request.

~~~text
stillwatch-cli cameras --data-dir DATA add --config CAMERA_JSON --key OPERATION_ID
stillwatch-cli cameras --data-dir DATA list
~~~

For replay, place a supported video in the Core sources directory. Enable pace for source-clock playback and repeat for repeated playback. A repeat creates a fresh source epoch. Replay incidents cannot initiate real alert delivery.

Core pauses analysis of exact repeated pixels after one second of confirmed repetition. After six seconds the camera reports that visual changes are unconfirmed. Native timestamps, snapshots and playback remain available while the source continues delivering valid frames. Analysis resumes with a changed image in the current source epoch. Transport or source-clock failures have separate status. A static digital scene can repeat exactly. Noise or changing overlays can hide a frozen picture from this check.

The owner can also select **Upload replay video** in the Companion. The Core probes a decoded frame before publishing the uploaded file. Each upload may contain up to 128 MiB and the managed upload directory holds up to 2 GiB. The original recording is stored on the Core host. Camera masks apply when replay is decoded and shared. Include required source videos in the host's backup and remove unused uploads from the replay directory after removing their camera configurations.

Save and enable the camera before marking a controlled flame. Open **Camera settings**, choose **Add fire region**, and tap the corners in order around the expected flame. Choose the source and expected flame mode, then apply and save. After changing the source or privacy masks, save those changes and review the regions on a fresh masked snapshot. Core requires another review when it detects a change in the camera geometry.

In **Camera settings → Analysis sampling**, the owner can set pose to 3–5 fps, fire and smoke to 1–2 fps and evidence to 1–5 fps. The defaults are 5, 2 and 5 fps. Lower rates can delay recognition or leave fewer pre-event frames. Core restarts that camera's capture and shows the rate achieved against its configured target. Evidence packets record any gaps in the sequence.

Set mediamtx, mediamtx_sha256, media_encoder, media_encoder_sha256 and media_ice_udp in core.json to enable video. Use the installed MediaMTX and FFmpeg/OpenH264 binaries and their digests from manifest.json. On Linux you can set STILLWATCH_ICE_UDP to enable video with the installed binaries. Bind ICE to the Core's private address. Verify decoded video separately from control connectivity.

For a camera with two RTSP profiles, enter the smaller one in **RTSP analysis stream path** and the higher-resolution one in **RTSP live detail path**. The ONVIF picker can populate either path. Use the same camera IP, port, transport, credential reference, framing and orientation for the two profiles. The source probe shows both codecs and resolutions. It rejects different aspect ratios when masks are configured. Check the framing on the camera after changing its profiles so the same normalized masks cover the intended areas in both streams.

Preview WHEP, snapshots and evidence use the analysis stream. Detail WHEP opens one shared decoder on demand and stops it after the last viewer closes. The Core applies masks before encoding H.264. If a detail connection fails, the Companion can still show snapshots from the analysis stream. The `media_detail_frame_bytes` setting in `core.json` sets a separate budget for RGB frames in the Core. The default is 128 MiB, with three frames reserved per active detail publisher. A detail offer returns HTTP 429 if the reservation would exceed this budget. Native decoder and encoder memory is additional.

Core passes the TURN signing secret to the MediaMTX child through its environment and writes a hash of the random loopback read credential to its private configuration. Startup also removes abandoned router configuration files from earlier runs under the Core data directory. Check that directory's permissions if startup logs report a cleanup failure. [MediaMTX environment configuration](https://mediamtx.org/docs/features/configuration)

The Companion rotates an active WHEP session after 25 minutes, before the Core's 30-minute expiry. When playback stalls, the Companion can show a masked snapshot with the age of the frame. It continues fetching snapshots while the camera and control route are reachable. Use **Reconnect video** after the media route recovers.

Device credential refresh replaces the previous access token immediately. Use the current token when submitting a WHEP offer after gathering ICE candidates. Core rechecks the device session before returning the stream grant, SDP answer or JPEG. HTTP 401 means the credential is invalid, expired or revoked. HTTP 503 with `storage_unavailable` means Core could not read session storage. A failed recheck closes any newly issued stream grant or negotiated stream.

Call `capabilities()` on either Rust SDK client to verify the public bootstrap. This request omits the bearer token. A paired client checks the protocol major version and the paired Core ID. An incompatible Core or an unexpected ID ends bootstrap with an error. If the asynchronous client reaches its final attempt and receives HTTP 401, it returns the server's rejection.

## Migration, backup and restore

Stop the legacy writer and take an offline backup of its data directory. Provide the first camera mapping as a JSON object with a camera field containing a CameraConfig. Set same_geometry to true only after verifying the camera framing and normalized privacy masks. The imported camera starts disabled. Review its source and masks before enabling it.

~~~text
stillwatch-cli migrate --legacy-data LEGACY_COPY --destination NEW_DATA --config CAMERA_MAPPING --dry-run
stillwatch-cli migrate --legacy-data LEGACY_COPY --destination NEW_DATA --config CAMERA_MAPPING
stillwatch-cli backup-offline --data-dir NEW_DATA --destination BACKUP
stillwatch-cli restore --backup BACKUP --destination RESTORED_DATA
~~~

Compare the source snapshot digest and row counts from both migration runs. The migration manifest also records how many historical images are available and the digest of the new database. Archived assessments remain associated with their original provider. Previously open incidents require reassessment and the migration creates no notification intents. For cutover, use a separate directory for the new Core and keep the old backup for rollback. Restore data compatible with the previous binary when rolling back a schema change.

Each backup and restore defaults to a 4 GiB per-operation budget. The `--max-bytes` option overrides it. The check includes the database, manifest and evidence, and requires 64 MiB of additional free space on the destination volume at preflight. Backups and restores use a private `.stillwatch-archive` staging directory beside the requested destination. One operation holds its lock at a time. A retry clears an abandoned staged copy under that lock before checking available space. The destination becomes visible after metadata and evidence validation. Account for previously saved backups in the destination volume's quota.

Core persists accepted incident openings in `accepted-openings.sqlite3` inside its private data directory. This journal has its own writer and holds opening metadata while the history database is busy. Core removes each journal entry after the history commit. Keep the journal and its SQLite sidecar files in place during a restart. Core drains pending writes during shutdown and restores any remaining journaled openings in bounded batches on startup. Offline backup folds pending openings into the history database before creating the archive. Openings recovered after restart or committed after a camera scope change are marked delayed and remain available for review without a new live notification. Images depend on the original frame and evidence lifecycle. [SQLite atomic commit](https://sqlite.org/atomiccommit.html)

### Upgrade or roll back Windows Core

Keep the previous MSI and a copy of its CLI and runtime dependencies with the offline backup. Save the host settings, camera credentials and replay sources separately from the SQLite and evidence archive. Set the backup path to a private directory on your backup volume.

1. Open an elevated PowerShell session and stop the StillWatch Core service.

   ~~~powershell
   $data = Join-Path $env:ProgramData 'StillWatch'
   $cli = Join-Path $env:ProgramFiles 'StillWatch Core\bin\stillwatch-cli.exe'
   $archive = 'D:\StillWatchBackups\before-upgrade'
   Stop-Service -Name StillWatchCore
   & $cli backup-offline --data-dir $data --destination $archive
   ~~~

2. Install the higher-version MSI. Reboot Windows, then check the service and open a fresh incident and JPEG from a controlled RTSP source.
3. For a schema rollback, stop the service and uninstall the newer MSI. Move the newer data directory aside under a distinct name. Use the saved earlier CLI to restore the matching archive into the vacant data directory.

   ~~~powershell
   & 'D:\StillWatchBackups\previous-cli\stillwatch-cli.exe' restore --backup $archive --destination $data
   ~~~

4. Restore the saved host settings and source files. Apply the original data-directory ACL. The permissions below match a new MSI install under LocalService.

   ~~~powershell
   icacls.exe $data /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' '*S-1-5-19:(OI)(CI)F'
   ~~~

5. Install the earlier MSI and confirm that the camera analysis resumes. Restore preserves the Core identity and starts a new stream generation. Re-pair affected clients before resuming notification delivery.

### Upgrade or roll back Linux Core

Keep the installed version's package, model pack and settings together with its offline backup. Include the Core environment, configured settings file, camera credentials and replay media in your host backup. The CLI archive covers the database and its evidence blobs. Record which notification relay services are enabled so you can start them again after checking Core.

1. Stop `stillwatch.service` and any active `stillwatch-apns.service`. Use a new private directory for each offline backup.

   ~~~sh
   sudo install -d -m 0700 /var/backups/stillwatch
   sudo stillwatch-cli backup-offline --data-dir /var/lib/stillwatch --destination /var/backups/stillwatch/before-upgrade
   ~~~

2. Install the newer DEB with `sudo dpkg -i ./PACKAGE.deb` or the newer RPM with `sudo dnf upgrade ./PACKAGE.rpm`. Review your saved configuration and start `stillwatch.service`. Run the CLI doctor as `stillwatch`, then check a new incident and JPEG from an authorized test source.
3. To roll back across a schema change, stop the services again. Install the previous DEB or use `sudo dnf downgrade ./PACKAGE.rpm`. Move the upgraded data directory aside under a distinct name. Restore the matching backup with the previous version's CLI into the now-vacant data directory.

   ~~~sh
   sudo mv /var/lib/stillwatch /var/lib/stillwatch-before-rollback
   sudo stillwatch-cli restore --backup /var/backups/stillwatch/before-upgrade --destination /var/lib/stillwatch
   sudo chown -R stillwatch:stillwatch /var/lib/stillwatch
   sudo chmod 0700 /var/lib/stillwatch
   ~~~

4. Restore the saved host settings, credentials and source files where needed. Start Core and re-pair the devices. Restore keeps the Core identity, starts a new stream generation and expires queued notices from the backup. Check readiness and confirm the next live incident before restarting the notification relay.

Core retains evidence JPEGs for 24 hours by default. Set evidence_retention_hours before cutover to choose how long copied images should stay accessible. Retention applies to the original capture time. Expired evidence keeps its historical metadata and is shown as unavailable.

Resolved incidents and their related metadata age out after 30 days by default. Use `history_retention_days` to set the window before starting Core. Incident metadata stays available for at least the seven-day command retry window. Open incidents and any unexpired evidence remain accessible. The window also governs older configuration revisions and trust audit records. After older events are pruned, a Companion with an expired cursor refreshes its snapshot. Back up the Core before reducing retention.

Core defaults to a 2 GiB SQLite metadata limit. Set `database_quota_bytes` in `core.json` or pass `--database-quota-bytes` to override it. The minimum is 4 MiB. SQLite reuses pages freed by retention. Existing database files larger than a newly configured limit remain readable and report low capacity. Before lowering the limit, make an offline backup and size the limit against the existing file.

Core attempts a non-blocking truncate checkpoint after the WAL reaches 16 MiB. One transaction can take it above that threshold. A reader holding an old SQLite snapshot can delay the checkpoint. In that case Core keeps reads available, rejects writes that require SQL changes with HTTP 507, and retries the checkpoint. Release the reader to restore writes. Metadata capacity pressure returns HTTP 503 from `/health/ready` while the process itself remains live. Check **Core health history** for database and free-space events. The operating system and the service account's volume quota still determine total disk capacity. [SQLite database and journal pragmas](https://sqlite.org/pragma.html), [SQLite WAL checkpointing](https://sqlite.org/wal.html)

## Operator checks

- In `GET /api/v2/status`, inspect `inference.ready_workers` and `inference.failure` together. Core reports `local_attention_ceiling: "review"` when at least one local inference worker is ready. The field is absent while every worker is starting, disabled, or unavailable.
- Run `stillwatch-cli doctor --data-dir DATA --output diagnostics.json` on the Core host to save readiness, inference worker and backend state, media router state, camera runtime, semantic provider status, and active host health codes from the current journal page. Camera entries use report-local numbers. The file excludes host addresses, instance and camera IDs, camera names, credentials, and images. Treat unavailable analysis as unknown. If the worker reports a model pack or runtime startup error, correct the configured path, digest, or execution provider and restart Core.
- In a camera viewer, check both the timestamp and the decoded picture. Loss of a private VPN or UDP path can affect video separately from the control API.
- Camera cards report unavailable analysis or a sustained rate below the current operating targets. The camera API reports accepted pose and fire results per minute over the trailing ten seconds of source time. The field appears after ten seconds of usable source time in the current epoch and worker generation. The defaults yield operating targets of 4 pose and 1.5 fire results per second, allowing for sampling against source timestamps. Other rates use 80% of the configured pose rate and 75% of the fire rate. Compare the measured rate with the recording cadence, then reduce active cameras or provision more inference capacity if the rate stays low.
- The optional camera priority advances eligible work in a worker's rotation by up to 150 ms. Waiting cameras gain priority as their last result ages. A due fire check that has missed its rate target takes precedence. A recent unresolved live fire or smoke candidate can use spare slots for up to two seconds at up to three fire checks per second, with a ten-second cooldown. Analysis status reports queue age at dispatch and the count of accepted extra fire results.
- For CPU deployments, `--inference-workers` controls the number of supervised worker processes. `--inference-threads` sizes the ONNX Runtime pool shared by the model sessions in each worker. Use `tools/benchmark/qualify.py` with the deployment's camera count and source geometry when choosing a profile. Each additional worker loads its own model sessions. [ONNX Runtime thread management](https://onnxruntime.ai/docs/performance/tune-performance/threading.html)
- Open **Settings → Core health history** in Companion. Use **Active** for current failures and **All events** for observed start and end times. Refresh to check a new state. The journal checks capture, source timestamps, inference, model verification, the semantic route, its daily budget and free space on the Core data volume. Six seconds of sustained measured low cadence start a camera interval. Six seconds of sustained measured recovery end it. It checks disk space every minute and flags less than 512 MiB available to Core. A system clock step over five seconds relative to monotonic time starts an interval. Thirty stable seconds end it. Camera rows follow device camera permissions. Ended rows follow the configured history retention and a rolling sequence window. Disabling or deleting a camera ends its active intervals.
- Take a schema-7 backup before first starting the health journal build against existing data. It upgrades to schema 8. Roll back with the original backup and a schema-7 binary.
- Run stillwatch-alarm-agent monitor from a second authorized host to check the Core independently. It emits changes in control-path health. Check media separately.
- In Companion, **Foreground notifications** shows the date and time this device observed a denied permission. Returning from system settings refreshes the permission. The timestamp stays on the device. The Companion uses foreground polling in this profile. Test the operating system's sound route and interruption behavior for each actual device.
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

Use `flutter build apk --release --target-platform android-arm64` and `flutter build appbundle --release --target-platform android-arm64`. The release build refreshes Android's plugin registrant to exclude the development-only integration test plugin. Android release signing reads the local, ignored key.properties file. Keep a stable app signing key for direct APK installs and updates. Sign Play bundles with the upload key registered for that app. The Play account uses its app signing identity for the APKs it delivers. Keep private signing material outside the checkout and record the signer fingerprints for each release. [Android app signing](https://developer.android.com/studio/publish/app-signing)

The iOS workflow builds the unsigned arm64 device archive with the locked Flutter toolchain and retains it for local signing. Check its `source-commit.txt` and run `sha256sum -c SHA256SUMS` before extracting the archive in Linux or WSL. The archive preserves the native QR scanner, notification delegate, Keychain plugin and WebRTC framework.

On Linux or WSL, put `rcodesign` 0.29.0, OpenSSL 3 and Zip on PATH. Use Apple's PEM root certificates, a StillWatch development or ad hoc provisioning profile covering the target iPhone, and its matching PKCS#12 identity. Keep the inputs and output in private storage. Set `$signing` to the directory holding the signing inputs. [Apple PKI](https://www.apple.com/certificateauthority/) [Registered device distribution](https://developer.apple.com/documentation/xcode/distributing-your-app-to-registered-devices) [Apple Codesign 0.29.0](https://github.com/indygreg/apple-platform-rs/releases/tag/apple-codesign%2F0.29.0)

```bash
python3 deploy/packaging/ios-sign.py \
  --app Runner.xcarchive/Products/Applications/Runner.app \
  --profile "$signing/stillwatch.mobileprovision" \
  --identity "$signing/stillwatch.p12" \
  --password-file "$signing/password" \
  --apple-roots "$signing/apple-roots.pem" \
  --device-file "$signing/iphone-udid" \
  --output StillWatch.ipa > StillWatch.signing.json
```

The JSON records the profile type, expiry, eligible device count and IPA checksum. Run `xtool devices --no-wait` and `xtool install StillWatch.ipa` on the signing host with the authorized iPhone connected. Verify pairing, playback, profile restoration and notifications on the phone. [xtool on Windows or Linux](https://xtool.sh/documentation/xtool/installation-linux/)

### iPhone background alerts

Enable Push Notifications for the exact StillWatch App ID in Apple Developer and create a provisioning profile that authorizes `aps-environment`. Pass `--require-apns` to `ios-sign.py` for a bundle that needs background alerts. The signing report records the environment. Use the sandbox endpoint with a development profile and the production endpoint with an ad-hoc profile. Keep the Apple signing identity and the APNs provider key in separate private files. [Apple APNs registration](https://developer.apple.com/documentation/usernotifications/registering-your-app-with-apns)

For TestFlight, use `--app-store` in place of `--device-file`. Provide an Apple Distribution identity and an App Store Connect profile for `com.stillwatch.stillwatchCompanion`. Upload the signed IPA to the matching app record in App Store Connect. TestFlight becomes available after Apple processes the build. [App Store profile](https://developer.apple.com/help/account/provisioning-profiles/create-an-app-store-provisioning-profile/) · [Build upload](https://developer.apple.com/help/app-store-connect/manage-builds/upload-builds/)

Build or install `stillwatch-apns-worker`. Run it on the Core host using a dedicated OS identity, a topic-scoped Apple APNs P-256 key, and a random 32-byte worker credential encoded as 64 hex characters. Give each process its own private copy of that credential. Keep the provider key under the worker identity and the app signing identity on the signing host. The worker accepts authenticated connections on loopback and connects outbound to Apple over HTTP/2 with TLS. [APNs connection and requests](https://developer.apple.com/documentation/usernotifications/sending-notification-requests-to-apns), [APNs token authentication](https://developer.apple.com/documentation/usernotifications/establishing-a-token-based-connection-to-apns)

~~~text
stillwatch-apns-worker --listen 127.0.0.1:8776 --delivery-token-file WORKER_CREDENTIAL --apple-key-file APPLE_KEY.p8 --team-id APPLE_TEAM_ID --key-id APPLE_KEY_ID --environment sandbox
stillwatch-core --data-dir DATA_DIR --apns-worker 127.0.0.1:8776 --apns-token-file CORE_WORKER_CREDENTIAL
~~~

On Linux the optional `stillwatch-apns.service` reads `LISTEN`, `DELIVERY_TOKEN_FILE`, `APPLE_KEY_FILE`, `TEAM_ID`, `KEY_ID` and `APNS_ENVIRONMENT` from a private `/etc/stillwatch/apns-worker.conf`. Set `apns_worker` and `apns_token_file` in Core's settings file for service installations. On Windows use ACLs that restrict each credential to its service identity.

Pair the iPhone, then enable **iOS background notifications** for that Core and allow system notifications. Registration needs a currently paired session and can expire after 14 days without reopening Companion. On each activation the app can refresh it. Set up the phone's pinned TLS or VPN route to Core so that tapping a notification can open the incident. APNs receives the delivery token, expiry and an opaque handle with generic alert text. The app resolves the handle only through the paired Core. A retry uses the same collapse identifier. Core records provider acceptance, and Companion sends a receipt when it verifies the notification in iOS or opens it. Open the incident in Companion to review or acknowledge it.

These are opt-in incident review notifications. The iOS worker sends an ordinary visible APNs alert with the system default sound. Focus, silent mode and notification permissions govern presentation and sound. The Apple agreement requires consent and treats APNs delivery as best effort. Verify presentation and sound on the intended physical devices before using background alerts. [Apple agreement, Attachment 1](https://developer.apple.com/support/terms/apple-developer-program-license-agreement/) · [Apple notification behavior](https://developer.apple.com/design/human-interface-guidelines/managing-notifications)

### Android background alerts

The user starts the monitoring service in Companion and can stop it from its persistent status notification. The service uses the paired Core connection over the configured LAN, VPN or relay. A loss of that route delays new alerts until the connection recovers. For a Play release, disclose the manifest's `specialUse` service type, explain the effect of interruption and provide the monitoring walkthrough for review. [Android foreground service types](https://developer.android.com/develop/background-work/services/fgs/service-types) · [Play Console declaration](https://support.google.com/googleplay/android-developer/answer/13392821)

Delivery is subject to the phone's notification settings, Focus, connectivity, Apple service conditions and expiration. After expiry or revocation the Core rejects the handle. A notification already shown by iOS can remain in its notification center. Test the supported use on a physical phone before relying on background alerts. [Apple delivery behavior](https://developer.apple.com/documentation/usernotifications/sending-notification-requests-to-apns)

## CUDA qualification

The CUDA backend uses the same locked FP32 model tensors and policies. Pass cuda_device, cuda_provider_sha256 and cuda_shared_sha256 on the Core command line with the existing model and ORT flags. Use dashes in flag names. In core.json use a cuda object with device, provider_sha256 and shared_sha256. Keep the provider libraries beside the matching ONNX Runtime library and install the corresponding NVIDIA user-space runtime through an authorized source. An invalid or missing provider prevents the requested backend from starting.

Use the CPU profile for qualified local operation. The CUDA Compose override mounts operator-provisioned CUDA, cuDNN and ONNX Runtime libraries read-only. It uses the NVIDIA Container Toolkit's CDI device mapping with Docker Engine 28.3 or later. Docker reads the generated CDI specification from its configured directories. The WSL profile uses `nvidia.com/gpu=all`. On Linux, set `STILLWATCH_GPU_DEVICE` to the desired device name from `nvidia-ctk cdi list` when the container must use a specific GPU. Run the worker's model verification with `--cuda DEVICE PROVIDER_SHA256 SHARED_SHA256` before relying on this backend. Tensor or observation drift outside the existing bounds leaves the profile experimental. [NVIDIA CDI support](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/cdi-support.html), [Docker CDI devices](https://docs.docker.com/reference/cli/docker/container/run/#cdi-devices)

Build the CUDA profile from the local verified CPU image and use both Compose files. The CUDA image configures its writable kernel cache under the existing `/tmp` tmpfs. The worker warms one tensor geometry before reporting readiness, within a bounded startup deadline. The live inference deadline stays two seconds.

~~~sh
docker build --pull=false -f deploy/packaging/Dockerfile.cuda -t stillwatch-core:1.0.0-cuda .
docker compose -f deploy/packaging/compose.cpu.yaml -f deploy/packaging/compose.cuda.yaml up -d
~~~

## Inference diagnostics

The existing `cadence` field in camera analysis status counts results over ten seconds of native source time. `wall_cadence` reports accepted Pose and Fire results during the trailing 60 monotonic seconds after a full window is available. It continues across source reconnects and worker restarts so an outage remains in the window. Capture reports the signed `relative_source_lag_ms` on live sources and paced replays after at least one second of native PTS progress and two seconds since the anchor receive. The initial delay before that anchor and absolute sensor age are unknown.

After a sustained relative lag, the camera reports `source_lagging` and analysis reports degraded. Local observations and video remain available. A delayed frame cannot support an urgent escalation, semantic dispatch, or automatic recovery. The health signal accounts for up to 100 ppm of source clock drift. Video and analysis coverage can be inspected independently in the local status response.

Owners can read `GET /api/v2/cameras/{id}/analysis-metrics` for disjoint latency buckets and per-task counters. Subtract two snapshots from the same camera and Core process to measure an interval. Each bucket has an inclusive upper bound in microseconds and the last bucket contains values above the highest bound. The first snapshot is available after dispatch. Each task anchors base slots at its first dispatch in a capture generation. Source epoch changes preserve counts and start new task clocks. `missed_source_slots` counts skipped base slots before dispatch and rejected base results. Use wall cadence to include an ongoing outage without new dispatches. Extra Fire samples preserve the base phase and require spare time before the next base slot on the lane. Metrics are retained for admitted cameras, and bounded process totals remain in the owner's `/api/v2/status` until Core exits.

## Evidence freshness

An incident can report the host receive time of its opening observation in `opening_observed_host_utc` and flag a delayed opening with `opening_delayed`. Evidence retains `observed_host_utc`, its original expiry, and a separate `persisted_at` timestamp sampled immediately before the committing transaction completes. `fresh_when_persisted` describes the clock and deadline checks made when the packet was stored. Core rechecks freshness, the current packet version and camera scope before using a semantic result. Packets from before an upgrade or restart stay readable in history. After restart, only newly opened incidents are eligible for a Firebase check.

The incident ledger retains the latest 64 material observations per incident, with native source time, host receive time, and the local facts and region. Core uses a per-camera image budget with a burst of three packets and one token replenished every four monotonic seconds. It gives new incidents and material worsening priority, retries unavailable evidence on fresh frames and refreshes unresolved observations after ten seconds when the budget permits. A newer material observation makes an earlier packet ineligible for a current semantic decision.
