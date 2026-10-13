#!/usr/bin/env bash
# H-9: the R0 baseline run (SPEC section 12).
#
#   bench/b1/run-r0.sh [--quick] [--rundirs ROOT]
#
# R0 = NullPolicy + the shared base (every focus miss fetches; nothing prefetched; the cache serves
# revisits). Build `gorca-daemon` from the commit that contains PW-1..PW-12 with the default
# (NullPolicy) fill, then:
#   * plan: step=7, walk=5, ff=5, idle=3, big=3   (SPEC 8.3; soak is run by the reviewer only)
#   * arm label `r0`; results in bench/results/b1-mirror-fill/r0/<UTC>.{json,txt}
#   * the walk driver comes from B1_WALK_DRIVER (H-3); until it exists use --rundirs or
#     `--plan step=7` (the step floor needs no daemon)
# Afterwards paste the SPEC section 12 table with:
#   target/release/b1 baseline-md bench/results/b1-mirror-fill/r0/<UTC>.json r0
set -euo pipefail
cd "$(dirname "$0")/../.."
daemon="${B1_R0_DAEMON:-$PWD/target/release/gorca-daemon}"
rev="$(git rev-parse --short HEAD)"
exec bench/b1/run-all.sh r0 --arm "r0:${daemon}:${rev}" "$@"
