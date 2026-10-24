#!/usr/bin/env bash
# Reproducible guest build: one component per name, copied to $OUT (default harness/build/guests),
# sha256 list in $OUT/SHA256SUMS. Needs `. ./env.sh` first. Never builds in /tmp.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/var/tmp/gorca-shared-target}"
export CARGO_INCREMENTAL=0
out="${OUT:-$here/../build/guests}"
mkdir -p "$out"
names="signal-p2 dummy-1 dummy-2 dummy-3 dummy-4 dummy-5 dummy-6 dummy-7 dummy-8 dummy-9 \
a1-page-stride a2-pointer-chase a3-huge-string a3b-huge-assert a3c-huge-attr a4-huge-return \
a5-cold-load a6-full-grant a13-subtree-loop a7-patch-match a8-refresh-each a12-self"
cd "$here"
for n in $names; do
  GUEST="$n" RUSTFLAGS="--remap-path-prefix=$here=/guest ${GUEST_RUSTFLAGS:-}" \
    cargo build --release --target wasm32-wasip2 --quiet
  cp "$CARGO_TARGET_DIR/wasm32-wasip2/release/m18a0_guest.wasm" "$out/$n.wasm"
done
(cd "$out" && sha256sum ./*.wasm > SHA256SUMS)
echo "built $(ls "$out"/*.wasm | wc -l) guests into $out"
