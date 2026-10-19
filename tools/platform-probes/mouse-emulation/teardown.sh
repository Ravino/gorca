#!/usr/bin/env bash
# teardown.sh WORKDIR : stop ONLY what the rig recorded at spawn (WORKDIR/pids, *.pid, rig.pgid). Never matches by name or cmdline.
set -u
W="$1"
for f in "$W"/pids; do [ -f "$f" ] && while read -r p; do kill "$p" 2>/dev/null; done < "$f"; done
for f in "$W"/web.pid "$W"/reg.pid; do [ -f "$f" ] && kill "$(cat "$f")" 2>/dev/null; done
[ -f "$W/rig.pgid" ] && kill -TERM -- "-$(cat "$W/rig.pgid")" 2>/dev/null
exit 0
