#!/usr/bin/env bash
# test-mapping.sh [SCALE] [WORKDIR] [TOOLKITS...] : MO-5a end to end in a PRIVATE headless shell (XWayland on). For each
# toolkit (default: gtk4 gtk3 gtk3x firefox chromium electron) a real client shows a "Target" button; AT-SPI gives its extents and
# the toplevel's; the Rust helper client matches the window and clicks; the button must fire. SCALE sets monitor 0's scale
# (1, 1.25, 2; 2 needs a 1920x1200 monitor, handled here). Build first:
#   . ./env.sh && cargo build -p gorca-shell-helper --example pointer_cli      (CARGO_TARGET_DIR is honoured)
# Teardown kills only recorded PIDs (tools/platform-probes/mouse-emulation/teardown.sh).
set -u
D="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCALE="${1:-1}"; W="${2:-${TMPDIR:-/tmp}/gorca-mapping-$$}"; shift 2 2>/dev/null
TK="${*:-gtk4 gtk3 gtk3x firefox chromium electron}"
BIN="${CARGO_TARGET_DIR:-$D/../../target}/debug/examples/pointer_cli"
[ -x "$BIN" ] || { echo "build the example first: $BIN not found"; exit 2; }
TD="$D/../../tools/platform-probes/mouse-emulation/teardown.sh"
trap '"$TD" "$W"' EXIT
export GORCA_RIG_EXTRA_EXE="$BIN" GORCA_RIG_NO_X11=
VM="1200x900 1280x800"; [ "$SCALE" = 2 ] && VM="1920x1200 1280x800"
setsid "$D/test-rig.sh" "$W" "$VM" >/dev/null 2>&1 < /dev/null &
for _ in $(seq 1 60); do sleep 1; grep -q "v2 owns" "$W/shell.log" 2>/dev/null && break; done
grep -q "v2 owns" "$W/shell.log" || { echo "rig did not start (see $W/shell.log)"; exit 2; }
. "$W/env"
if [ "$SCALE" != 1 ]; then
  /usr/bin/python3 "$D/../../tools/platform-probes/mouse-emulation/apply_scale.py" "$BUS" 0 "$SCALE" | tail -1
  sleep 2
fi
"$D/run-as.sh" "$W" a "$D/testclients/test_mapping.py" $TK
