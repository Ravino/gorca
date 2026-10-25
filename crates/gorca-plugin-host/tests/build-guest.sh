#!/usr/bin/env bash
# Build the test guest component. Usage: build-guest.sh <out.wasm>. Needs `. ./env.sh` first.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export CARGO_INCREMENTAL=0
tgt="${GUEST_TARGET_DIR:-${CARGO_TARGET_DIR:-/var/tmp/gorca-shared-target}/plugin-test-guest}"
cd "$here/guest"
CARGO_TARGET_DIR="$tgt" RUSTFLAGS="--remap-path-prefix=$here=/guest" \
  cargo build --release --target wasm32-wasip2 --quiet
cp "$tgt/wasm32-wasip2/release/plugin_test_guest.wasm" "$1"
