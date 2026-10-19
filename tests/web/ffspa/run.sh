#!/bin/bash
# Firefox SPA bugs (chat list Enter after Escape, Ctrl+C on a chat-app page) with REAL keys in a
# PRIVATE headless GNOME Shell (tests/shell/run_shell.sh). Reuses tests/web/selcopy/inner.sh.
#   GORCA_BIN=target/release/gorca [FFSPA_PAGE=tgapp.html?v=rerender] [FFSPA_DRIVER=drive_chat.py] tests/web/ffspa/run.sh
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
export GORCA_PASSTHROUGH="RDFIFO GORCA_BIN GORCA_CFG MARKLOG SELCOPY_BROWSER SELCOPY_OUT SELCOPY_DRIVER SELCOPY_URL RUST_LOG FFSPA_PAGE FFSPA_SEQ FFSPA_CFG FFSPA_COPY"
export SELCOPY_URL="file://$HERE/${FFSPA_PAGE:-tgapp.html}"
export SELCOPY_DRIVER="$HERE/${FFSPA_DRIVER:-drive_chat.py}"
exec "$ROOT/tests/shell/run_shell.sh" -- "$ROOT/tests/web/selcopy/inner.sh"
