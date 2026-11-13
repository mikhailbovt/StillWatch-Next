#!/usr/bin/env bash
set -euo pipefail

if (( $# != 3 )); then
    echo "Usage: rpm.sh STAGED_PAYLOAD EMPTY_RPMBUILD_ROOT OUTPUT_DIRECTORY" >&2
    exit 2
fi
source_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
payload=$(realpath "$1")
top=$(realpath -m "$2")
output=$(realpath -m "$3")
if [[ -e "$top" && -n $(find "$top" -mindepth 1 -maxdepth 1 -print -quit) ]]; then
    echo "Use an empty rpmbuild directory" >&2
    exit 2
fi
install -d "$top/SOURCES" "$top/SPECS" "$output"
tar -C "$payload" -czf "$top/SOURCES/stillwatch-payload.tar.gz" .
install -m 0755 "$source_root/deploy/linux/stillwatch-core" "$top/SOURCES/stillwatch-core"
install -m 0644 "$source_root/deploy/linux/stillwatch.conf" "$top/SOURCES/core.conf"
install -m 0644 "$source_root/deploy/systemd/stillwatch.service" "$top/SOURCES/stillwatch.service"
cat > "$top/SPECS/stillwatch-core.spec" <<'EOF'
Name: stillwatch-core
Version: 0.1.0
Release: 1
Summary: StillWatch Next autonomous monitoring service
License: Apache-2.0 AND MIT AND LGPL-2.1-or-later AND BSD-2-Clause
BuildArch: x86_64
Requires: systemd, shadow-utils, gnutls
Source0: stillwatch-payload.tar.gz
Source1: stillwatch-core
Source2: core.conf
Source3: stillwatch.service
%global _build_id_links none
%global debug_package %{nil}
# The release files and native libraries are already stripped or vendor-built.
# Preserve their bytes so the installed manifest still verifies.
%global __os_install_post %{nil}

%description
Native Rust Core, capture and inference worker, and desktop alarm agent.

%install
install -d %{buildroot}/usr/libexec/stillwatch %{buildroot}/usr/bin
tar -xzf %{SOURCE0} -C %{buildroot}/usr/libexec/stillwatch
install -m 0755 %{SOURCE1} %{buildroot}/usr/bin/stillwatch-core
install -D -m 0644 %{SOURCE2} %{buildroot}/etc/stillwatch/core.conf
install -D -m 0644 %{SOURCE3} %{buildroot}/usr/lib/systemd/system/stillwatch.service
ln -s ../libexec/stillwatch/bin/stillwatch-cli %{buildroot}/usr/bin/stillwatch-cli

%pre
getent group stillwatch >/dev/null || groupadd -r stillwatch
getent passwd stillwatch >/dev/null || useradd -r -g stillwatch -d /var/lib/stillwatch -s /sbin/nologin -c "StillWatch service" stillwatch

%post
install -d -m 0700 -o stillwatch -g stillwatch /var/lib/stillwatch
if test -d /run/systemd/system; then systemctl daemon-reload; fi

%preun
if test "$1" = 0 && test -d /run/systemd/system; then systemctl disable --now stillwatch.service; fi

%postun
if test -d /run/systemd/system; then systemctl daemon-reload; fi

%files
%config(noreplace) /etc/stillwatch/core.conf
%dir /etc/stillwatch
/usr/bin/stillwatch-core
/usr/bin/stillwatch-cli
/usr/libexec/stillwatch
/usr/lib/systemd/system/stillwatch.service
EOF
rpmbuild -bb --define "_topdir $top" --define "_rpmdir $output" "$top/SPECS/stillwatch-core.spec"
