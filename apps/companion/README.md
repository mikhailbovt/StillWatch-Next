# StillWatch Companion

Connect to a running StillWatch Core to view cameras, inspect incidents and acknowledge alerts. Enroll with a one-time pairing offer from the Core host. Desktop clients can paste the offer JSON. Android and iOS can scan the QR code.

Incident actions queued while offline stay pending in the current app session for up to two minutes. Use **Verify and send** after reconnecting. Companion checks the updated incident revision and evidence before submitting the action to the Core.

Use Flutter 3.50.0 and the checked-in package lock for development.

~~~sh
flutter pub get --enforce-lockfile
flutter analyze --fatal-infos
flutter test --no-pub
~~~

Use `flutter build linux` or `flutter build windows` on the corresponding host. The Windows build requires an account allowed to create developer symlinks and the Visual Studio C++ desktop toolchain with CMake, Windows SDK and C++ ATL for v143 x86/x64 (`Microsoft.VisualStudio.Component.VC.ATL`). Build Android with JDK 21 and Android SDK platform 37. Configure a private release keystore before distributing an APK or AAB. Run `flutter build ios --release --no-codesign` on macOS to compile without signing. Installation on a physical iPhone requires the corresponding Apple signing identity and provisioning.

See docs/operations.md in the repository for Core setup, pairing and diagnostics.
