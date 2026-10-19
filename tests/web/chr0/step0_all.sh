#!/bin/bash
# Step 0 of CHR-0: the slow-load page and its control on Chrome 155, Chromium 154 and Electron 44.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
P=$HERE/../m8/pages
for k in chrome chromium electron; do
  for v in slowload-auto slowload-auto-ctl; do
    [ -s "$HERE/../../../docs/reports/chr0-step0/$k-$v.jsonl" ] && continue
    "$HERE/run_probe.sh" "$k-$v" "$P/$v.html" "$k"
  done
done
