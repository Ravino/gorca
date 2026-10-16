#!/bin/bash
# M9 runner: the M7 / M8 / typing e2e sets against a PRIVATE Chromium (Chrome for Testing or a
# distro chromium) instead of Firefox. Same private worlds as tests/web/m7/run_m7.sh (a headless
# gnome-shell on a unique Wayland socket + a private a11y bus); nothing touches the live session.
#
#   tests/web/m9/run_m9.sh m7 <label> [e2e options]     M7a set + typing set (e2e --only ...)
#   tests/web/m9/run_m9.sh m8 <label> [e2e-m8 options]  the NVDA parity corpus
#   tests/web/m9/run_m9.sh probe <mode> [page] [title]  how Chromium finds out an AT runs
#   tests/web/m9/run_m9.sh probe-load <page|URL> [title]  tree arrival and event rate while a page loads
#
# env:
#   M9_CHROMIUM   browser binary (default: the Playwright Chrome for Testing in ~/.cache/ms-playwright,
#                 else `chromium` from PATH)
#   M9_AT         how Chromium learns that an AT runs (the a11y bus `IsEnabled` flag is always set,
#                 `ScreenReaderEnabled` never, as gorca does):
#                   isenabled  nothing else: the pure state of main (the web tree never appears,
#                              see docs/reports/m9-baseline.md section 2)
#                   poke       (default) the AT driver calls GetAttributes/GetRelationSet on the
#                              application node once, which is the wake-up call gorca must send
#                   orca       poke + a decoy process with argv[0]=orca (Chromium's /proc scan)
#                   force      --force-renderer-accessibility
#   M9_ARGS       extra Chromium arguments
#   M7_KEEP=1     keep the compositor world's temp dir
# Chromium runs with a throw-away profile that is deleted when the browser stops.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
KIND=${1:-}
shift || true
CHR=${M9_CHROMIUM:-}
if [ -z "$CHR" ]; then
  CHR=$(ls -d "$HOME"/.cache/ms-playwright/chromium-*/chrome-linux64/chrome 2>/dev/null | tail -1)
  [ -n "$CHR" ] || CHR=$(command -v chromium || true)
fi
[ -x "$CHR" ] || { echo "no Chromium found: set M9_CHROMIUM" >&2; exit 2; }
export GORCA_M7_BROWSER=chromium
export GORCA_M7_CHROMIUM=$CHR
export GORCA_M9_AT=${M9_AT:-isenabled}
export GORCA_M9_CHROMIUM_ARGS=${M9_ARGS:-}
# gorca writes IsEnabled only; the harness must not set ScreenReaderEnabled either
export GORCA_HARNESS_SCREEN_READER=0
case "$KIND" in
  m7) exec "$ROOT/tests/web/m7/run_m7.sh" "$@" ;;
  m8) exec "$ROOT/tests/web/m8/run_m8.sh" "$@" ;;
  probe)
    MODE=${1:-poke}; PAGE=${2:-$ROOT/tests/web/m7/pages/article.html}; TITLE=${3:-M7 Article}
    exec "$ROOT/tests/web/m7/run_m7.sh" --probe /usr/bin/python3 "$ROOT/tests/web/m9/probe_at.py" "$MODE" "$PAGE" "$TITLE" ;;
  probe-load)
    exec "$ROOT/tests/web/m7/run_m7.sh" --probe /usr/bin/python3 "$ROOT/tests/web/m9/probe_load.py" "$@" ;;
  probe-scn)
    exec "$ROOT/tests/web/m7/run_m7.sh" --probe /usr/bin/python3 "$ROOT/tests/web/m9/probe_scn.py" "$@" ;;
  probe-typing)
    exec "$ROOT/tests/web/m7/run_m7.sh" --probe /usr/bin/python3 "$ROOT/tests/web/m9/probe_typing.py" "$@" ;;
  *) echo "usage: $0 m7|m8|probe ..." >&2; exit 64 ;;
esac
