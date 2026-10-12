#!/bin/bash
# usage: run_chrome_atspi.sh <page.html> [atspi_tuned_walk args...]
# Private session+a11y bus, private headless Chrome with --force-renderer-accessibility. Never touches user buses/browsers.
HERE=$(cd $(dirname $0) && pwd); PAGE=$1; shift
CH=${CHROME:-$HOME/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome}
. $HERE/private_a11y_env.sh up
PROF=$(mktemp -d /tmp/chprof.XXXX)
$CH --headless=new --no-sandbox --force-renderer-accessibility --disable-gpu --user-data-dir=$PROF file://$PAGE >/dev/null 2>&1 &
CPID=$!
sleep 6
/usr/bin/python3 $HERE/atspi_tuned_walk.py $A11Y_ADDR Chrome "$@"
echo "{\"step\":\"rss_mb\",\"chrome_total\":$(ps -eo rss,args | grep -F "$PROF" | grep -v grep | awk '{s+=$1} END {print int(s/1024)}')}"
kill $CPID 2>/dev/null; sleep 1; ps -eo pid,args | grep -F "$PROF" | grep -v grep | awk '{print $1}' | xargs -r kill 2>/dev/null
. $HERE/private_a11y_env.sh down; rm -rf $PROF
