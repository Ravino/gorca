#!/usr/bin/env python3
"""insert_offsets.py - how far is a text-changed Insert offset from the caret event that follows it?

  insert_offsets.py <reader.log of a run with RUST_LOG=gorca_text=debug>

Counts, over every `edit event ... Insert{offset,len}` directly followed by a `caret event offset=N`,
the difference `offset + len - N` (0 = the event names the place the text went; 1, 2 = the offset
is past the caret: the Gecko `gecko-insert-offset` quirk; negative = before it).
"""
import re
import sys
from collections import Counter

ANSI = re.compile(r"\x1b\[[0-9;]*m")
ins = re.compile(r"edit event .*delta=Insert \{ offset: (\d+), len: (\d+)")
car = re.compile(r"caret event .* offset=(\d+)")
last = None
hist = Counter()
for line in open(sys.argv[1], errors="replace"):
    line = ANSI.sub("", line)
    m = ins.search(line)
    if m:
        last = (int(m.group(1)), int(m.group(2)))
        continue
    m = car.search(line)
    if m and last:
        hist[last[0] + last[1] - int(m.group(1))] += 1
        last = None
    elif "gorca_text" in line and "say" not in line:
        last = None
tot = sum(hist.values())
print("inserts followed by a caret event:", tot)
for k in sorted(hist):
    print("  offset+len-caret = %+d : %d (%.1f%%)" % (k, hist[k], 100.0 * hist[k] / tot))
