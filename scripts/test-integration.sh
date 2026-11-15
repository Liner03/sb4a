#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# End-to-end check of start/stop against a fake core. Must run as root inside a
# container: the CLI hardcodes /data/adb and refused to run otherwise.
set -euo pipefail

BIN="${1:?usage: test-integration.sh <path-to-sb4a>}"
MOD=/data/adb/modules/sb4a
CLI=$MOD/bin/sb4a

rm -rf /data/adb
mkdir -p "$MOD/bin" "$MOD/assets"
printf 'id=sb4a\nname=sb4a\nversion=v0.1\nversionCode=1\n' > "$MOD/module.prop"

# Stand-in for the Go rootbox: same argv contract, writes `ready`, streams stats.
cat > "$MOD/bin/rootbox" <<'CORE'
#!/bin/sh
if [ "$1" = "--check" ]; then
  [ -f "$2" ] || { echo "missing config: $2" >&2; exit 1; }
  # Stand-in for sing-box's strict decoder.
  grep -q '"nope"' "$2" && { echo "rootbox: decode config: unknown field" >&2; exit 1; }
  exit 0
fi
[ -f "$1" ] || { echo "missing config: $1" >&2; exit 1; }
echo $$ > "$3"
: > "$4"
n=0
while [ ! -f "$5" ]; do
  n=$((n + 1))
  echo "WANBOX_STATS:{\"tag\":\"proxy\",\"tx\":$n,\"rx\":2,\"directTx\":3,\"directRx\":4}"
  sleep 1
done
exit 0
CORE
chmod +x "$MOD/bin/rootbox"
install -m 0755 "$BIN" "$CLI"

step() { printf '== %s\n' "$1"; }

step "validate rejects a config the core refuses"
cat > /tmp/bad.json <<'JSON'
{"schemaVersion":1,"profileId":8,"profileName":"bad","config":{"route":{"nope":true}}}
JSON
if printf '%s' "$(cat /tmp/bad.json)" | "$CLI" config validate | grep -q '"ok":true'; then
  echo "FAIL: broken config accepted" >&2
  exit 1
fi

step "apply"
"$CLI" config apply <<'JSON' | tee /tmp/apply.json
{"schemaVersion":1,"profileId":7,"profileName":"fixture","config":{"log":{"level":"warn"}}}
JSON
grep -q '"ok":true' /tmp/apply.json

step "start (returns once connected, not when the core exits)"
timeout 60 "$CLI" start | tee /tmp/start.json
grep -q '"phase":"connected"' /tmp/start.json

step "stats survive one throttle interval"
sleep 6
"$CLI" status | tee /tmp/status.json
grep -q '"stats":{"tag":"proxy"' /tmp/status.json

step "stop"
"$CLI" stop | tee /tmp/stop.json
grep -q '"phase":"stopped"' /tmp/stop.json

step "boot starts without blocking on readiness"
"$CLI" autostart on > /dev/null
timeout 20 "$CLI" __internal boot | grep -q '"ok":true'
for _ in $(seq 1 100); do
  "$CLI" status | grep -q '"phase":"connected"' && break
  sleep 0.2
done
"$CLI" status | tee /tmp/boot.json
grep -q '"phase":"connected"' /tmp/boot.json
"$CLI" stop > /dev/null

step "boot is a no-op when autostart is off"
"$CLI" autostart off > /dev/null
"$CLI" __internal boot | grep -q '"phase":"stopped"'

echo "INTEGRATION OK"
