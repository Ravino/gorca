#!/bin/bash
# Browse-mode selection -> the browser's own Ctrl+C -> clipboard, end to end in a PRIVATE
# headless GNOME Shell (tests/shell/run_shell.sh). Nothing touches the live session.
#
#   GORCA_BIN=target/debug/gorca [SELCOPY_BROWSER=firefox|chromium] [GORCA_CFG=gorca.toml] \
#     tests/web/selcopy/run.sh
#
# Select with gorca's browse keys (Shift+arrows, Ctrl+Shift+arrows, Shift+End, Ctrl+A), press
# Ctrl+C (the browser copies), read the clipboard back through the RemoteDesktop session, and
# paste into the page's own text field. Exit status 1 when a PASS/FAIL row fails.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
export GORCA_PASSTHROUGH="RDFIFO GORCA_BIN GORCA_CFG MARKLOG SELCOPY_BROWSER SELCOPY_OUT SELCOPY_DRIVER"
exec "$ROOT/tests/shell/run_shell.sh" -- "$HERE/inner.sh"
