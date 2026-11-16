#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Build rootbox (the sing-box integration layer) as an Android arm64 PIE.
#
# GOOS=android keeps the bionic loader (PT_INTERP /system/bin/linker64); CGO is
# off so no NDK is needed and the result links no bionic libraries.
set -euo pipefail

cd "$(dirname "$0")/../go"

OUT="${OUT_DIR:-../target/android/arm64-v8a}"
# hysteria/tuic need QUIC, reality needs uTLS, wireguard needs its own stack;
# tailscale brings its endpoint, DERP and tailcat. The ts_omit_* tags drop
# the client parts a root module never uses (logtail, ssh, drive, ...).
TAGS="${TAGS:-with_quic,with_utls,with_wireguard,with_gvisor,with_tailscale,with_dhcp,with_grpc,with_clash_api,ts_omit_logtail,ts_omit_ssh,ts_omit_drive,ts_omit_taildrop,ts_omit_webclient,ts_omit_doctor,ts_omit_capture,ts_omit_kube,ts_omit_aws,ts_omit_synology,ts_omit_bird}"
export GOPROXY="${GOPROXY:-https://goproxy.cn,direct}"
mkdir -p "$OUT"

if command -v go >/dev/null 2>&1; then
  CGO_ENABLED=0 GOOS=android GOARCH=arm64 \
    go build -trimpath -tags "$TAGS" -ldflags='-s -w' -o "$OUT/rootbox" .
else
  docker run --rm -v "$(cd .. && pwd)":/work -v sb4a-go:/go -w /work/go \
    -e GOPROXY -e CGO_ENABLED=0 -e GOOS=android -e GOARCH=arm64 \
    "${BUILD_IMAGE:-golang:1.25-bookworm}" \
    go build -trimpath -tags "$TAGS" -ldflags='-s -w' -o /work/target/android/arm64-v8a/rootbox .
fi

file "$OUT/rootbox"
ls -lh "$OUT/rootbox"
