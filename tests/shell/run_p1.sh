#!/bin/bash
# P1: keypress-to-speech check of the wired focus and change path, in a private Shell world.
#   tests/shell/run_p1.sh OUTDIR [GORCA_BINARY] [shell|gtk4|firefox ...]
# Nothing here touches the live session (tests/shell/run_shell.sh -> tests/harness/run-isolated.sh).
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=${1:?usage: run_p1.sh OUTDIR [GORCA_BINARY] [targets]}
shift
mkdir -p "$OUT"
exec "$HERE/run_shell.sh" -- /usr/bin/python3 "$HERE/p1_wired.py" "$OUT" "$@"
