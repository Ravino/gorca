#!/usr/bin/env bash
# Wrapper around the reader for the B-5 judge: runs "$@", forwards SIGTERM/SIGINT to it and, after it
# exits, writes the CPU seconds it and its threads consumed (user and system) to $GORCA_CPU_OUT.
# Only the child this script started is signalled (CLAUDE.md live-session safety).
# The harness asks the reader binary for its build revision (`--version`): answer as the real one.
if [ "${1:-}" = --version ]; then exec "$(dirname "$0")/../../target/release/gorca" --version; fi
"$@" &
child=$!
trap 'kill -TERM "$child" 2>/dev/null' TERM INT
rc=0
wait "$child" || rc=$?
# the trap interrupts the first wait; wait again for the real exit
wait "$child" 2>/dev/null || rc=$?
if [ -n "${GORCA_CPU_OUT:-}" ]; then
  # `times` prints "shell user shell sys" then "children user children sys" as XmY.ZZZs
  # appended: one line per reader start of the run (the harness starts the reader per scenario group)
  tmp=$GORCA_CPU_OUT.$$
  times >"$tmp" 2>/dev/null && tail -n1 "$tmp" >>"$GORCA_CPU_OUT"
  rm -f "$tmp"
fi
exit "$rc"
