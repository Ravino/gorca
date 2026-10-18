#!/bin/bash
# Stand-in browser binary for the M8/M9 e2e rig (GORCA_M7_CHROMIUM). Rewrites the page URL (last
# argument) to a copy whose body content is removed at DOMContentLoaded and put back after
# WRAP_DELAY_MS, then execs WRAP_BIN ("electron" = the tiny Electron app, or a browser path).
# Config: $ELECTRON_PROBE_DIR/wrap.conf (key=value lines; the rig's env allowlist drops env vars).
set -u
. "$(dirname "$0")/common.sh"
OUT=$EP/pages; mkdir -p -m 700 "$OUT"
args=("$@"); last=${args[-1]}
case "$last" in
  file://*)
    src=${last#file://}; dst=$OUT/$(basename "$src")
    if [ "$WRAP_DELAY_MS" -gt 0 ]; then
      sed "0,/<head>/s##<head><script>document.addEventListener('DOMContentLoaded',function(){var b=document.body,k=Array.from(b.childNodes).slice(8);k.forEach(function(n){n.remove()});setTimeout(function(){k.forEach(function(n){b.appendChild(n)})},${WRAP_DELAY_MS})})</script>#" "$src" > "$dst"
      args[-1]="file://$dst"
    fi ;;
esac
if [ "$WRAP_BIN" = electron ]; then
  exec "$EP/dist2/chromelectron" --no-sandbox --disable-gpu "$EP/app" "${args[@]}"
else
  exec "$WRAP_BIN" "${args[@]}"
fi
