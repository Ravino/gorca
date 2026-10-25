#!/usr/bin/env bash
# Flat-review line check of the review cut-over (M22b3): the line model of a page of mixed roles
# (labels before entries, controls that hold a label of their own name, links, list items, tabs,
# menu items, cells) built by the legacy tree (the tag legacy/review-last) and by this tree, and
# diffed. The same ignored test runs in both (`review::latency_tests::dump_line_texts_of_the_mixed_page`).
#
#   bench/m6/review-lines.sh [BASELINE_REF]
set -euo pipefail
here=$(cd "$(dirname "$0")/../.." && pwd)
base_ref=${1:-legacy/review-last}
cd "$here"
. ./env.sh
export CARGO_INCREMENTAL=0
new_target=${CARGO_TARGET_DIR:-/var/tmp/gorca-shared-target}
old_target=/var/tmp/gorca-m6-baseline-target
wt=$(mktemp -d /var/tmp/gorca-m6-review-lines.XXXXXX)
cleanup() { git -C "$here" worktree remove --force "$wt" >/dev/null 2>&1 || true; }
trap cleanup EXIT
git worktree add --detach "$wt" "$base_ref" >/dev/null
ln -s "$here/.toolchain" "$wt/.toolchain"
out=$(mktemp -d)
dump() { # dir target file
  (cd "$1" && CARGO_TARGET_DIR="$2" cargo test --release -p gorca-core --lib dump_line_texts_of_the_mixed_page -- --ignored --nocapture 2>&1 \
    | sed -n '/^LINES-BEGIN$/,/^LINES-END$/p' | sed '1d;$d' >"$3")
}
dump "$wt" "$old_target" "$out/legacy.txt"
dump "$here" "$new_target" "$out/new.txt"
echo "lines: legacy $(wc -l <"$out/legacy.txt"), new $(wc -l <"$out/new.txt")"
if diff -u "$out/legacy.txt" "$out/new.txt"; then
  echo "review lines: IDENTICAL"
else
  echo "review lines: DIFFER (see the diff above)"
  exit 1
fi
