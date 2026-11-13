#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
destination=${1:-"$root/.local/native"}
mkdir -p "$destination"
destination=$(realpath "$destination")
mapfile -t spec < <(python3 - "$root/models/native-deps.lock.json" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as stream:
    entry = json.load(stream)["ort"]["linux-x64"]
for key in ("url", "archive_sha256", "library", "library_sha256"):
    print(entry[key])
PY
)
test "${#spec[@]}" -eq 4
archive="$destination/${spec[0]##*/}"
library="$destination/${spec[2]}"
if ! test -f "$library"; then
    if ! test -f "$archive"; then
        curl -fsSL --retry 3 -o "$archive" "${spec[0]}"
    fi
    printf '%s  %s\n' "${spec[1]}" "$archive" | sha256sum -c >&2
    tar -xzf "$archive" -C "$destination"
fi
printf '%s  %s\n' "${spec[3]}" "$library" | sha256sum -c >&2
printf '%s\n' "$library"
