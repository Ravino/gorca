#!/bin/bash
# Runs every chat-app scenario with real keys in private sessions; prints one PASS/FAIL line each.
#   GORCA_BIN=target/release/gorca tests/web/ffspa/run_all.sh
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
rc=0
run() { # label page driver [seq]
  local out; out=$(FFSPA_PAGE="$2" FFSPA_DRIVER="$3" FFSPA_SEQ="${4:-A}" "$HERE/run.sh" 2>&1)
  if echo "$out" | grep -q '^FAIL'; then echo "FAIL $1"; echo "$out" | grep '^FAIL'; rc=1; else echo "PASS $1"; fi
}
run "bug1: Enter opens a chat after open+Escape+Tab (div rows)"        "tgapp.html?v=keyed&row=div" drive_chat.py A
run "bug1: same with unkeyed re-render"                                 "tgapp.html"                 drive_chat.py A
run "bug1: Enter on a row the page rewrites every second (2 presses)"  "tgapp.html?v=keyed&churn=1"  drive_chat.py E
run "bug2: Ctrl+C with a contenteditable composer focused"             "tgapp.html"                 drive_copy.py
run "bug2: Ctrl+C with an input composer focused"                      "tgapp.html?c=input"         drive_copy.py
run "bug2: Ctrl+C with a textarea composer focused"                    "tgapp.html?c=textarea"      drive_copy.py
exit $rc
