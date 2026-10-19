#!/bin/bash
# run_probe.sh <label> <page.html> <browser|electron> - CHR-0 step 0: growth timeline of a page that
# holds its load open, in a private session (see probe_growth.py). Results go to
# docs/reports/chr0-step0/<label>.jsonl (kept, also when the run fails).
#
#   browser   chrome | chromium | electron    (electron: ~/.cache/electron-probe, mode 700)
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
cd "$ROOT" || exit 1
. ./env.sh
LABEL=$1; PAGE=$(readlink -f "$2"); KIND=$3
OUT=$ROOT/docs/reports/chr0-step0
mkdir -p "$OUT"
case "$KIND" in
  chrome)   FRAG=chrome;  CMD=(/opt/google/chrome/chrome --user-data-dir=@PROFILE@ --ozone-platform=wayland --no-first-run --no-default-browser-check "file://$PAGE") ;;
  chromium) FRAG=chromium; CMD=(/usr/bin/chromium --user-data-dir=@PROFILE@ --ozone-platform=wayland --no-first-run --no-default-browser-check "file://$PAGE") ;;
  electron) FRAG=electron; CMD=("$HOME/.cache/electron-probe/dist2/chromelectron" --no-sandbox --disable-gpu --ozone-platform=wayland --user-data-dir=@PROFILE@ "$HOME/.cache/electron-probe/app" "file://$PAGE") ;;
  *) echo "unknown browser $KIND" >&2; exit 64 ;;
esac
"$HERE/with_slow_server.sh" "$ROOT/tests/web/m7/run_m7.sh" --probe /usr/bin/python3 "$HERE/probe_growth.py" "$FRAG" poke -- "${CMD[@]}" \
  > "$OUT/$LABEL.jsonl" 2> "$OUT/$LABEL.err"
echo "rc=$?" >> "$OUT/$LABEL.err"
