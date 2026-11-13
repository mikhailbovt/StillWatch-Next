#!/usr/bin/env bash
set -euo pipefail

core=$(realpath "$1")
unit="stillwatch-next-qualification-$$"
data=$(mktemp -d /tmp/stillwatch-next-systemd.XXXXXX)
cleanup() {
    systemctl stop "$unit" 2>/dev/null || true
    rm -r -- "$data"
}
trap cleanup EXIT

port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
systemd-run --quiet --collect --unit="$unit" \
    -p Type=notify -p WatchdogSec=2s -p Restart=on-failure -p RestartSec=200ms \
    "$core" --data-dir "$data" --listen "127.0.0.1:$port"

address="http://127.0.0.1:$port"
identity=$(curl -fsS "$address/bootstrap/info")
first_pid=$(systemctl show "$unit" -p MainPID --value)
sleep 5
test "$(systemctl show "$unit" -p MainPID --value)" = "$first_pid"

systemctl kill --signal=KILL --kill-whom=main "$unit"
for _ in {1..50}; do
    current_pid=$(systemctl show "$unit" -p MainPID --value)
    if test "$current_pid" != 0 && test "$current_pid" != "$first_pid" && test "$(systemctl show "$unit" -p ActiveState --value)" = active; then
        break
    fi
    sleep 0.1
done
test "$current_pid" != "$first_pid"
test "$current_pid" != 0
test "$(curl -fsS "$address/bootstrap/info")" = "$identity"
systemctl stop "$unit"
printf 'systemd readiness, watchdog, crash restart and identity persistence verified\n'
