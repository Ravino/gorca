#!/bin/bash
# The deciding e2e pair: M8-SLOW on Electron 44 with main's reader and with the CHR-0 reader.
# On Electron the tree content of a loading page stays out of the platform tree until the load
# ends, so a mirror built early is stale for good unless the end of the load repairs it.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
BASE=${CHR0_BASE_BIN:?set CHR0_BASE_BIN to a copy of the main gorca binary}
T=${CHR0_TAG:-r4}
export GORCA_M8_DUMP=1
CHR0_SERVER_LOG=/tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/server-$T-base.log \
  E2E_EXTRA="--repeats 1" "$HERE/run_e2e.sh" electron "chr0-$T-base-electron" M8-SLOW "$BASE"
CHR0_SERVER_LOG=/tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/server-$T-new.log \
  E2E_EXTRA="--repeats 1" "$HERE/run_e2e.sh" electron "chr0-$T-new-electron" M8-SLOW
