#!/bin/bash
# Runs inside the private shell world (see run.sh): a browser, gorca in browse mode, key
# injection through a Mutter RemoteDesktop session (rd2.py), and the driver script.
#   env: GORCA_BIN (daemon), GORCA_CFG (settings file, optional), SELCOPY_BROWSER=firefox|chromium
set -u
F=$(cd "$(dirname "$0")" && pwd)
OUT=${SELCOPY_OUT:-$(mktemp -d /tmp/gorca-selcopy.XXXXXX)}
mkdir -p "$OUT"
export GDK_BACKEND=wayland MOZ_ENABLE_WAYLAND=1
PROF=$(mktemp -d /tmp/gorca-selcopy-prof.XXXXXX)
FIFO=$OUT/rd.fifo; rm -f "$FIFO"; mkfifo "$FIFO"; exec 7<>"$FIFO"
/usr/bin/python3 "$F/rd2.py" <&7 >"$OUT/rd.log" 2>&1 &
RDP=$!
export RDFIFO=$FIFO MARKLOG=$OUT/marks.log
sleep 2
export XDG_CONFIG_HOME=$OUT/cfg; mkdir -p "$XDG_CONFIG_HOME/gorca"
[ -n "${GORCA_CFG:-}" ] && cp "$GORCA_CFG" "$XDG_CONFIG_HOME/gorca/gorca.toml"
/usr/bin/python3 "$F/../../shell/ssip_log.py" "$OUT/ssip.sock" "$OUT/ssip.log" &
SP=$!
sleep 1
RUST_LOG=${RUST_LOG:-warn} "${GORCA_BIN:?set GORCA_BIN}" --impersonate-orca --no-enable --speech=ssip \
  --ssip-socket="$OUT/ssip.sock" --a11y-address="$AT_SPI_BUS_ADDRESS" >"$OUT/gorca.log" 2>&1 &
GP=$!
sleep 3
case "${SELCOPY_BROWSER:-firefox}" in
  firefox) firefox -profile "$PROF" --no-remote --new-instance "file://$F/page.html" >"$OUT/browser.log" 2>&1 & ;;
  chromium) chromium --user-data-dir="$PROF" --force-renderer-accessibility --ozone-platform=wayland \
      --no-first-run --no-default-browser-check "file://$F/page.html" >"$OUT/browser.log" 2>&1 & ;;
esac
BP=$!
sleep 14
echo "key 125" >"$FIFO"      # leave the Overview a headless shell starts in
sleep 2
echo "click" >"$FIFO"
sleep 3
/usr/bin/python3 "${SELCOPY_DRIVER:-$F/drive.py}"
rc=$?
kill "$BP" "$GP" "$SP" "$RDP" 2>/dev/null
sleep 1
rm -rf "$PROF"
echo "outputs in $OUT" >&2
exit $rc
