#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Pack the Magisk module zip from the two Android binaries.
set -euo pipefail

cd "$(dirname "$0")/.."

BIN="${BIN_DIR:-target/android/arm64-v8a}"
OUT="${OUT:-target/sb4a-module-arm64.zip}"

for name in sb4a rootbox; do
  [ -f "$BIN/$name" ] || { echo "missing $BIN/$name" >&2; exit 1; }
done

staging="$(mktemp -d)"
trap 'rm -rf "$staging"' EXIT

for name in LICENSE THIRD-PARTY-NOTICES.md; do
  [ -f "$name" ] || { echo "missing $name" >&2; exit 1; }
  cp "$name" "$staging/"
done
cp -R module/. "$staging/"
mkdir -p "$staging/bin"
cp "$BIN/sb4a" "$BIN/rootbox" "$staging/bin/"
chmod 0700 "$staging/bin/sb4a" "$staging/bin/rootbox"

# README stays in the repo: module managers keep only known files.
(cd "$staging" && zip -qr - .) > "$OUT"
ls -lh "$OUT"
