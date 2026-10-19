#!/bin/bash
# matrix.sh OUTDIR - the Orca / gorca comparison on the Signal-like page (about 12 private runs, ~80 s each).
# Needs: /usr/bin/orca, a gorca release binary (SC_GORCA_BIN, default <repo>/target/release/gorca),
# the Electron probe dir (tools/electron-probe, binary dist2/signal-desktop: a hard link of chromelectron).
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=${1:?outdir}
mkdir -p "$OUT"
export SC_SETTLE=${SC_SETTLE:-2.5}
export SC_APPNAME=${SC_APPNAME:-signal-desktop}
run() { # reader app scenario label
  echo "== $4 $(date +%T) load $(cut -d' ' -f1 /proc/loadavg)"
  timeout 420 "$HERE/run.sh" "$1" "$2" "$3" "$OUT/$4" > "$OUT/$4.out" 2>&1
  echo "   rc=$?"
}
for app in electron chromium; do
  for scen in list chat chat_focus; do
    for reader in orca gorca; do
      run $reader $app $scen "${reader}_${app}_${scen}"
    done
  done
done
echo done
