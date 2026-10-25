#!/bin/bash
# P3: keypress-to-speech check of the wired review and object-navigation path, in a private Shell world.
#   tests/shell/run_p3.sh OUTDIR [GORCA_BINARY] [shell|gtk4|firefox ...]
# Nothing here touches the live session (tests/shell/run_shell.sh -> tests/harness/run-isolated.sh).
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=${1:?usage: run_p3.sh OUTDIR [GORCA_BINARY] [targets]}
shift
mkdir -p "$OUT"
exec "$HERE/run_shell.sh" -- /usr/bin/python3 "$HERE/p3_review.py" "$OUT" "$@"
