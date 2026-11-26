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
    ort = json.load(stream)["ort"]
package = ort["cpu_nuget"]
entry = ort["linux-x64"]
print(package["url"])
print(package["archive_sha256"])
print(package["members"]["linux-x64"])
print(entry["library"])
print(entry["library_sha256"])
PY
)
test "${#spec[@]}" -eq 5
archive="$destination/${spec[0]##*/}"
library="$destination/${spec[3]}"
if ! test -f "$library"; then
    if ! test -f "$archive"; then
        partial=$(mktemp -- "$destination/.ort-download.XXXXXX")
        trap 'rm -f -- "$partial"' EXIT
        curl -fsSL --retry 3 -o "$partial" "${spec[0]}"
        printf '%s  %s\n' "${spec[1]}" "$partial" | sha256sum -c >&2
        mv -f -- "$partial" "$archive"
    else
        printf '%s  %s\n' "${spec[1]}" "$archive" | sha256sum -c >&2
    fi
    python3 - "$archive" "${spec[2]}" "$library" "${spec[4]}" <<'PY'
import hashlib
from pathlib import Path
import shutil
import sys
import tempfile
from zipfile import ZipFile

archive, member, target, expected = sys.argv[1:]
library = Path(target)
notice_dir = library.parent.parent
library.parent.mkdir(parents=True, exist_ok=True)

with ZipFile(archive) as package:
    for name, path in (
        ("LICENSE", notice_dir / "LICENSE"),
        ("ThirdPartyNotices.txt", notice_dir / "ThirdPartyNotices.txt"),
        (member, library),
    ):
        with package.open(name) as source, tempfile.NamedTemporaryFile(
            dir=path.parent, delete=False
        ) as temporary:
            candidate = Path(temporary.name)
            try:
                shutil.copyfileobj(source, temporary)
                temporary.flush()
                if path == library:
                    with candidate.open("rb") as contents:
                        if hashlib.file_digest(contents, "sha256").hexdigest() != expected:
                            raise ValueError("ONNX Runtime library digest mismatch")
                candidate.chmod(0o644)
                candidate.replace(path)
            finally:
                candidate.unlink(missing_ok=True)
PY
fi
printf '%s  %s\n' "${spec[4]}" "$library" | sha256sum -c >&2
printf '%s\n' "$library"
