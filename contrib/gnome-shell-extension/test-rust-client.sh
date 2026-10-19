#!/usr/bin/env bash
# test-rust-client.sh [WORKDIR] [PATH_TO_pointer_cli] : the Rust ShellHelperClient (examples/pointer_cli) against the
# REAL helper in a PRIVATE headless shell (own bus, own Wayland display), with a GTK4 client as the oracle.
# Build the example first: `. ./env.sh && cargo build -p gorca-shell-helper --example pointer_cli`.
# Teardown kills only the PIDs the rig recorded (tools/platform-probes/mouse-emulation/teardown.sh).
set -u
D="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
W="${1:-${TMPDIR:-/tmp}/gorca-rustclient-$$}"
BIN="${2:-${CARGO_TARGET_DIR:-$D/../../target}/debug/examples/pointer_cli}"
[ -x "$BIN" ] || { echo "build the example first: $BIN not found"; exit 2; }
TD="$D/../../tools/platform-probes/mouse-emulation/teardown.sh"
trap '"$TD" "$W"' EXIT
export GORCA_RIG_EXTRA_EXE="$BIN"
setsid "$D/test-rig.sh" "$W" "1280x800 1200x900" >/dev/null 2>&1 < /dev/null &
for _ in $(seq 1 60); do sleep 1; grep -q "v2 owns" "$W/shell.log" 2>/dev/null && break; done
grep -q "v2 owns" "$W/shell.log" || { echo "rig did not start (see $W/shell.log)"; exit 2; }
"$D/run-as.sh" "$W" a "$D/testclients/test_rust_client.py"
