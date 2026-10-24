#!/usr/bin/env bash
# A5 proof: no openat/read/mmap of cache files on the writer thread. Usage: host/strace-writer.sh <arm>
# strace -f traces our own child only; the writer thread is named m18a0-writer (comm).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bin="${CARGO_TARGET_DIR:-/var/tmp/gorca-shared-target}/release/m18a0-run"
out="${OUT:-$here/../../results}"
mkdir -p "$out"
strace -f -ttt -e trace=openat,read,mmap -o "$out/a5-strace.txt" \
  "$bin" run --arm "$1" --scenario "$here/../scenarios/adv-a5.toml" --condition dev --samples 2000 --out "$out"
echo "inspect a5-strace.txt: lines of the m18a0-writer tid touching .cwasm must be absent"
