#!/bin/bash
# capture-all.sh - regenerate tests/fixtures/sem/{firefox,chromium,gtk4}/*.jsonl.
#
#   tools/sem-capture/capture-all.sh [--engines "firefox chromium gtk4"] [--pages "widgets forms"] [--out DIR]
#
# Everything runs in ONE private world made by tests/shell/run_shell.sh: private session bus,
# private a11y bus + registryd, and a private headless gnome-shell (unique Wayland display in a
# private XDG_RUNTIME_DIR) that gives the browsers and the GTK4 gallery a display. Nothing touches
# the live session. Each app is started in its own session (setsid) by capture.py and only that
# recorded process group is signalled afterwards.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
FX=$ROOT/tests/fixtures/sem
# M8 corpus pages captured next to the A2 pages (page name gets an "m8-" prefix)
M8PAGES="forms tables landmarks live hidden quicknav flat shadow chatlist iframes"
# M8b pages (APG widgets, forms with error messages), captured as "m8b-<page>"
M8BPAGES="widgets forms"

if [ "${1:-}" = "__inner" ]; then
  shift
  ENGINES=$1; PAGES=$2; OUT=$3
  rc=0
  for eng in $ENGINES; do
    if [ "$eng" = gtk4 ]; then
      for pg in $(/usr/bin/python3 "$HERE/gtk4_gallery.py" --list); do
        [ -n "$PAGES" ] && ! echo " $PAGES " | grep -q " $pg " && continue
        timeout -k 5 180 /usr/bin/python3 -I "$HERE/capture.py" --engine gtk4 --page "$pg" --out "$OUT/gtk4" || rc=1
      done
    else
      for f in "$FX"/pages/*.html $(for m in $M8PAGES; do echo "$ROOT/tests/web/m8/pages/$m.html"; done) $(for m in $M8BPAGES; do echo "$ROOT/tests/web/m8b/pages/$m.html"; done); do
        pg=$(basename "$f" .html)
        case "$f" in */tests/web/m8/pages/*) pg="m8-$pg" ;; */tests/web/m8b/pages/*) pg="m8b-$pg" ;; esac
        [ -n "$PAGES" ] && ! echo " $PAGES " | grep -q " $pg " && continue
        early=""
        # Firefox 153 ESR dies about a second after a date/time/datetime-local input is exposed:
        # read the first readable tree of those pages instead of waiting for it to settle.
        case "$eng:$pg" in firefox:exotic-date|firefox:exotic-time|firefox:exotic-datetime) early="--early" ;; esac
        timeout -k 5 180 /usr/bin/python3 -I "$HERE/capture.py" --engine "$eng" --page "$pg" --html "$f" --out "$OUT/$eng" $early || rc=1
      done
    fi
  done
  exit $rc
fi

ENGINES="firefox chromium gtk4"; PAGES=""; OUT=$FX
while [ $# -gt 0 ]; do
  case "$1" in
    --engines) ENGINES=$2; shift ;;
    --pages) PAGES=$2; shift ;;
    --out) OUT=$2; shift ;;
    *) echo "unknown option $1" >&2; exit 64 ;;
  esac
  shift
done
exec "$ROOT/tests/shell/run_shell.sh" --no-unsafe -- "$HERE/capture-all.sh" __inner "$ENGINES" "$PAGES" "$OUT"
