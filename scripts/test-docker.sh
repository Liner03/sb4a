#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Host-safe test loop: no host network, no privileged mode, no port publishing.
# One-time image: docker build -t sb4a-test:1.83 scripts
# Prime deps:     docker run --rm -v "$PWD":/work -v sb4a-cargo:/usr/local/cargo \
#                   -w /work sb4a-test:1.83 cargo fetch
set -euo pipefail

docker run --rm \
  --network none \
  --cap-drop ALL \
  --security-opt no-new-privileges \
  -v "$(cd "$(dirname "$0")/.." && pwd)":/work \
  -v sb4a-cargo:/usr/local/cargo \
  -w /work \
  sb4a-test:1.83 \
  sh -c 'cargo fmt --check && cargo clippy --offline --all-targets -- -D warnings &&
         cargo test --offline && cargo check --offline --target aarch64-linux-android -p sb4a-cli'
