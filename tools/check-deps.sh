#!/usr/bin/env bash
# Enforce the ADR-0001 section 11 dependency rules over `cargo metadata`.
# Exits 0 when the workspace is clean, 1 on a forbidden edge (the chain is printed), 2 on error.
#   tools/check-deps.sh            check the real workspace
#   tools/check-deps.sh --selftest run the rule engine's own tests (synthetic bad graphs)
set -euo pipefail
cd "$(dirname "$0")/.."
# shellcheck disable=SC1091
. ./env.sh
case "${1:-}" in
  --selftest) exec cargo test --quiet -p xtask ;;
  "") exec cargo xtask deps ;;
  *) echo "usage: $0 [--selftest]" >&2; exit 2 ;;
esac
