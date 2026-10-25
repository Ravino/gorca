#!/bin/bash
# Telegram Web A / K fixtures with REAL keys in a PRIVATE headless GNOME Shell (tests/shell/run_shell.sh,
# tests/web/selcopy/inner.sh). Nothing touches the live session; the world is torn down by run-isolated.sh.
#   tests/web/telegram/run.sh A|K|E firefox|chromium SCENARIO OUTDIR [QUERY-FLAGS]
#   SCENARIO: echo login list chat scrollup listscroll incoming flood churn typing   (see tg_drive.py)
#   QUERY-FLAGS: appended to the URL, e.g. "?f=incoming,typing" or "?a11y=1"
#   TG_PLUGINS=on|off (default off): turn the plugin runtime on (`[plugins] enabled = true`, the
#   first-party plugins from plugins/dist, built by tools/build-plugins.sh) for the scenario `plugin`.
#   GORCA_BIN (default: scratch copy of the release binary) must be absolute.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
APP=${1:?A|K}; BR=${2:?firefox|chromium}; SCEN=${3:?scenario}; OUT=${4:?outdir}; Q=${5:-}
case "$APP" in A) PAGE=a.html ;; K) PAGE=k.html ;; E) PAGE=echo.html ;; *) echo "A, K or E (echo control page)" >&2; exit 2 ;; esac
mkdir -p "$OUT"; OUT=$(cd "$OUT" && pwd)
export GORCA_BIN=${GORCA_BIN:-<repo>/target/release/gorca}
export GORCA_PASSTHROUGH="RDFIFO GORCA_BIN GORCA_CFG MARKLOG SELCOPY_BROWSER SELCOPY_OUT SELCOPY_DRIVER SELCOPY_URL RUST_LOG TG_SCEN TG_BROWSER TG_PLUGINS TG_PAGE GORCA_PLUGINS_DIR GORCA_PLUGINS_FIRST_PARTY_DIR GORCA_PLUGINS_CACHE_DIR GORCA_PLUGINS_URL_EXTRA"
export TG_PLUGINS=${TG_PLUGINS:-off}
if [ "$TG_PLUGINS" = on ]; then
  printf '[plugins]\nenabled = true\n' >"$OUT/gorca-plugins.toml"
fi
# Article quick navigation has no default key; the scenario `plugin` binds it (on and off alike).
if [ "$SCEN" = plugin ]; then
  [ -f "$OUT/gorca-plugins.toml" ] || : >"$OUT/gorca-plugins.toml"
  printf '[keymap.browse]\n"a" = "quicknav.next.article"\n' >>"$OUT/gorca-plugins.toml"
  export GORCA_CFG=$OUT/gorca-plugins.toml
fi
if [ "$TG_PLUGINS" = on ]; then
  export GORCA_PLUGINS_FIRST_PARTY_DIR=$ROOT/plugins/dist GORCA_PLUGINS_DIR=$OUT/no-user-plugins GORCA_PLUGINS_CACHE_DIR=$OUT/cwasm
  # The fixtures are file:// pages; the manifest only knows web.telegram.org. Test knob, private session only.
  export GORCA_PLUGINS_URL_EXTRA="gorca.telegram-web=file://$HERE/*"
fi
export SELCOPY_BROWSER=$BR SELCOPY_OUT=$OUT SELCOPY_DRIVER=$HERE/tg_drive.py
export SELCOPY_URL="file://$HERE/$PAGE$Q"
export TG_SCEN=$SCEN TG_BROWSER=$BR TG_PAGE=$APP
"$ROOT/tests/shell/run_shell.sh" -- "$ROOT/tests/web/selcopy/inner.sh" 2>&1 | tee "$OUT/run.log"
exit "${PIPESTATUS[0]}"
