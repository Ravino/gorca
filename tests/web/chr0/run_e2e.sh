#!/bin/bash
# run_e2e.sh <engine> <label> <only> [reader-bin] - the CHR-0 / M8 scenarios with the slow server up.
#   engine: firefox | chrome | chromium | electron
#   only:   M8 scenario ids, comma separated (M8-SLOW,M8-SLOW0,M8-MSG)
#   reader-bin: gorca binary under test (default target/release/gorca); the baseline is a copy
#               of main's binary, so the same steps run against both.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
ENGINE=$1; LABEL=$2; ONLY=$3
[ -n "${4:-}" ] && export M8_READER_BIN=$4
case "$ENGINE" in
  firefox)  RUN=("$ROOT/tests/web/m8/run_m8.sh") ;;
  chrome)   export M9_CHROMIUM=/opt/google/chrome/chrome; RUN=("$ROOT/tests/web/m9/run_m9.sh" m8) ;;
  chromium) export M9_CHROMIUM=/usr/bin/chromium; RUN=("$ROOT/tests/web/m9/run_m9.sh" m8) ;;
  electron) export M9_CHROMIUM="$ROOT/tools/electron-probe/delay-wrapper.sh"; RUN=("$ROOT/tests/web/m9/run_m9.sh" m8) ;;
  *) echo "unknown engine" >&2; exit 64 ;;
esac
# E2E_EXTRA: more e2e-m8 options, for example "--repeats 1"
# shellcheck disable=SC2086
exec "$HERE/with_slow_server.sh" "${RUN[@]}" "$LABEL" --only "$ONLY" ${E2E_EXTRA:-}
