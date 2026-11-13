#!/usr/bin/env bash
set -euo pipefail

if (( $# != 3 )); then
    echo "Usage: debian.sh STAGED_PAYLOAD EMPTY_ROOT OUTPUT.deb" >&2
    exit 2
fi
source_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
payload=$(realpath "$1")
stage=$(realpath -m "$2")
output=$(realpath -m "$3")
if [[ -e "$stage" && -n $(find "$stage" -mindepth 1 -maxdepth 1 -print -quit) ]]; then
    echo "Use an empty Debian staging directory" >&2
    exit 2
fi
install -d "$stage/usr/libexec/stillwatch" "$stage/usr/bin" "$stage/DEBIAN" "$(dirname "$output")"
cp -a "$payload/." "$stage/usr/libexec/stillwatch/"
install -m 0755 "$source_root/deploy/linux/stillwatch-core" "$stage/usr/bin/stillwatch-core"
ln -s ../libexec/stillwatch/bin/stillwatch-cli "$stage/usr/bin/stillwatch-cli"
install -D -m 0644 "$source_root/deploy/systemd/stillwatch.service" "$stage/usr/lib/systemd/system/stillwatch.service"
install -D -m 0644 "$source_root/deploy/linux/stillwatch.conf" "$stage/etc/stillwatch/core.conf"

cat > "$stage/DEBIAN/control" <<'EOF'
Package: stillwatch-core
Version: 0.1.0
Architecture: amd64
Section: video
Priority: optional
Maintainer: StillWatch maintainers
Depends: libc6 (>= 2.39), libgcc-s1, libstdc++6, libgnutls30t64, adduser
Description: StillWatch Next autonomous monitoring service
 Native Rust Core, capture and inference worker, and desktop alarm agent.
EOF
printf '%s\n' /etc/stillwatch/core.conf > "$stage/DEBIAN/conffiles"
cat > "$stage/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -eu
if ! getent passwd stillwatch >/dev/null; then
    adduser --system --group --home /var/lib/stillwatch --no-create-home stillwatch
fi
install -d -m 0700 -o stillwatch -g stillwatch /var/lib/stillwatch
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload
fi
EOF
cat > "$stage/DEBIAN/prerm" <<'EOF'
#!/bin/sh
set -eu
if [ "$1" = remove ] && [ -d /run/systemd/system ]; then
    systemctl disable --now stillwatch.service
fi
EOF
cat > "$stage/DEBIAN/postrm" <<'EOF'
#!/bin/sh
set -eu
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload
fi
EOF
chmod 0755 "$stage/DEBIAN/postinst" "$stage/DEBIAN/prerm" "$stage/DEBIAN/postrm"
dpkg-deb --root-owner-group --build "$stage" "$output"
