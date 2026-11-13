#!/usr/bin/env bash
set -euo pipefail

if (( $# != 2 )); then
    echo 'Usage: installed_cpu.sh MODEL_DIRECTORY MANIFEST_SHA256' >&2
    exit 2
fi

export STILLWATCH_MODEL_DIR=$1 STILLWATCH_MODEL_SHA256=$2
install_dir=/usr/libexec/stillwatch
test "$(id -un)" = stillwatch
for unavailable in python python3 cargo; do
    if command -v "$unavailable" >/dev/null; then
        echo "Unexpected runtime command $unavailable" >&2
        exit 1
    fi
done

"$install_dir/bin/stillwatch-worker" capture-doctor | grep -F '"rtsp_peer_confined":true'
"$install_dir/bin/stillwatch-worker" doctor \
    "$install_dir/runtime/libonnxruntime.so.1.31.0" 469849977cb60744ea160927e1a3a0643fd4c55c0aff743251c6d293402e334e \
    "$STILLWATCH_MODEL_DIR" "$STILLWATCH_MODEL_SHA256" | grep -F '"models":3'
/usr/bin/stillwatch-core --check-inference | grep -F 'inference workers are ready'

/usr/bin/stillwatch-core &
core=$!
cleanup() {
    kill "$core" 2>/dev/null || :
    wait "$core" 2>/dev/null || :
}
trap cleanup EXIT
for attempt in {1..30}; do
    kill -0 "$core"
    if [[ -s /var/lib/stillwatch/local-access-token ]] &&
        timeout 5 /usr/bin/stillwatch-cli doctor --data-dir /var/lib/stillwatch \
            >/tmp/stillwatch-doctor.json 2>/tmp/stillwatch-doctor-error; then
        if grep -Fq '"ready_workers": 1' /tmp/stillwatch-doctor.json &&
            grep -Fq '"failure": null' /tmp/stillwatch-doctor.json; then
            grep -F '"state": "ready"' /tmp/stillwatch-doctor.json
            exit 0
        fi
    fi
    sleep 0.2
done
if [[ -f /tmp/stillwatch-doctor-error ]]; then
    cat /tmp/stillwatch-doctor-error >&2
fi
if { exec 3<>/dev/tcp/127.0.0.1/8765; } 2>/dev/null; then
    printf 'GET /health/live HTTP/1.1\r\nHost: 127.0.0.1:8765\r\nConnection: close\r\n\r\n' >&3
    timeout 3 head -n 1 <&3 >&2 || :
fi
echo 'Installed Core did not reach readiness' >&2
exit 1
