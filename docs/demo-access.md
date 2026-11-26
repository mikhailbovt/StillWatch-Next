# Try StillWatch

See a camera feed, an incident with its evidence and a notification arriving on a paired device in the [StillWatch video walkthrough](https://youtu.be/lM7q3qSZoeI).

## Install Companion

- **iPhone** · Open the public invitation on your iPhone. It takes you to TestFlight, where you can accept the invitation and install an available build. [Join the StillWatch beta](https://testflight.apple.com/join/EM8yb4Ex)
- **Windows, Linux and Android** · Choose the Windows installer, the Linux DEB or RPM, or the Android APK for your device. [Download Companion](https://github.com/mikhailbovt/StillWatch-Next/releases/tag/v1.0.0)

## Connect to a camera

StillWatch is self-hosted. The Core runs on the Windows or Linux computer connected to the cameras. To view a shared camera, ask its owner for a one-time pairing offer. The owner chooses which cameras and permissions to share.

1. Open Companion and select **Add a Core**. On iPhone or Android, select **Scan QR** and scan the owner's offer. You can also paste the offer JSON on any platform.
2. Compare the fingerprint shown under **Verify the Core** with the one the owner gives you, then finish pairing. Each offer is valid for five minutes and enrolls one device. If it expires, ask for a fresh offer.
3. Open **Cameras** to view a shared live stream, or **Incidents** to inspect a recorded event. Live video and evidence images require the corresponding permissions.

**Find nearby** fills in the address of a Core on your local network. You still need its pairing offer. For access from another network, the owner can configure a reachable address or the StillWatch relay. [Remote connections](remote.md)

## Run your own Core

Install Core, provision the local models and connect a camera or video replay. Enable a reachable TLS endpoint for Companion. Run the first owner pairing command on the Core computer. [Core downloads](https://github.com/mikhailbovt/StillWatch-Next/releases/tag/v1.0.1) · [Setup and pairing](operations.md#enroll-a-device) · [Model setup](operations.md#replace-a-model-pack)

**Windows** · Open an administrator PowerShell after installing the MSI.

```powershell
& "$env:ProgramFiles\StillWatch Core\bin\stillwatch-cli.exe" pair --data-dir "$env:ProgramData\StillWatch" --role owner
```

**Linux** · For a package installation using the default data directory, run the following command.

```sh
sudo -u stillwatch stillwatch-cli pair --data-dir /var/lib/stillwatch --role owner
```

Once your first device is paired, open **Settings → Pair device** to invite someone else. Select **Viewer**, choose the cameras they can see and share the new offer privately.
