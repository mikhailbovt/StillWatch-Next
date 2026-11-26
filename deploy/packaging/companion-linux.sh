#!/usr/bin/env bash
set -euo pipefail

if (( $# != 4 )) || [[ "$1" != deb && "$1" != rpm ]]; then
    echo "Usage: companion-linux.sh deb|rpm RELEASE_BUNDLE ALARM_AGENT OUTPUT_DIRECTORY" >&2
    exit 2
fi
format=$1
bundle=$(realpath "$2")
agent=$(realpath "$3")
output=$(realpath -m "$4")
stage=$(mktemp -d -t stillwatch-companion.XXXXXXXX)
trap 'rm -rf -- "$stage"' EXIT
test -x "$bundle/stillwatch_companion"
test -x "$agent"
if find "$bundle" -type f \( -name '*.onnx' -o -name '*.safetensors' -o -name '*.tflite' -o -name '*onnxruntime*' \) | grep -q .; then
    echo "Companion bundle contains an inference artifact" >&2
    exit 1
fi
install -d "$stage/tree/usr/libexec/stillwatch-companion" "$stage/tree/usr/bin" "$output"
cp -a "$bundle/." "$stage/tree/usr/libexec/stillwatch-companion/"
install -m 0755 "$agent" "$stage/tree/usr/bin/stillwatch-alarm-agent"
ln -s ../libexec/stillwatch-companion/stillwatch_companion "$stage/tree/usr/bin/stillwatch-companion"
install -d "$stage/tree/usr/lib/systemd/user"
cat > "$stage/tree/usr/lib/systemd/user/stillwatch-alarm-agent.service" <<'EOF'
[Unit]
Description=StillWatch desktop alarm agent
PartOf=graphical-session.target
ConditionPathExists=%S/stillwatch/alarm-agent.json

[Service]
ExecStart=/usr/bin/stillwatch-alarm-agent run --state "%S/stillwatch/alarm-agent.json" --audible
UMask=0077
Restart=on-abnormal
RestartSec=5s

[Install]
WantedBy=graphical-session.target
EOF
install -d "$stage/tree/usr/share/applications" "$stage/tree/usr/share/pixmaps"
install -m 0644 "$bundle/data/flutter_assets/assets/mark.png" "$stage/tree/usr/share/pixmaps/stillwatch-companion.png"
cat > "$stage/tree/usr/share/applications/stillwatch-companion.desktop" <<'EOF'
[Desktop Entry]
Name=StillWatch
Comment=StillWatch camera and incident console
Exec=stillwatch-companion
Icon=stillwatch-companion
Terminal=false
Type=Application
Categories=AudioVideo;Monitor;
EOF
if [[ "$format" = deb ]]; then
    mkdir "$stage/tree/DEBIAN"
    cat > "$stage/tree/DEBIAN/control" <<'EOF'
Package: stillwatch-companion
Version: 0.1.0
Architecture: amd64
Section: video
Priority: optional
Maintainer: StillWatch maintainers
Depends: libc6 (>= 2.39), libstdc++6, libgtk-3-0t64, libsecret-1-0, libjsoncpp25, libgstreamer1.0-0, libgstreamer-plugins-base1.0-0
Recommends: avahi-daemon, gnome-keyring, pulseaudio-utils
Description: StillWatch Next native Companion
 View cameras, review incidents and manage access to a paired Core.
EOF
    dpkg-deb --root-owner-group --build "$stage/tree" "$output/stillwatch-companion_0.1.0_amd64.deb"
else
    install -d "$stage/rpm/SOURCES" "$stage/rpm/SPECS"
    tar -C "$stage/tree" -czf "$stage/rpm/SOURCES/bundle.tar.gz" .
    cat > "$stage/rpm/SPECS/companion.spec" <<'EOF'
Name: stillwatch-companion
Version: 0.1.0
Release: 1
Summary: StillWatch Next native Companion
License: Apache-2.0 AND BSD-3-Clause
BuildArch: x86_64
Source0: bundle.tar.gz
%global _build_id_links none
%global debug_package %{nil}
%global __os_install_post %{nil}
%description
View cameras, review incidents and manage access to a paired Core.
%install
mkdir -p %{buildroot}
tar -C %{buildroot} -xzf %{SOURCE0}
%files
/usr/bin/stillwatch-companion
/usr/bin/stillwatch-alarm-agent
/usr/libexec/stillwatch-companion
/usr/lib/systemd/user/stillwatch-alarm-agent.service
/usr/share/applications/stillwatch-companion.desktop
/usr/share/pixmaps/stillwatch-companion.png
EOF
    rpmbuild -bb --define "_topdir $stage/rpm" --define "_rpmdir $output" "$stage/rpm/SPECS/companion.spec"
fi
