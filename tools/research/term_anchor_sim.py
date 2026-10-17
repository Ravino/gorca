#!/usr/bin/env python3
"""Terminal live-review anchoring simulator (research prototype, see
docs/reports/terminal-live-review-math.md).

A ground-truth terminal holds logical lines with true ids. Its accessible text
is what VTE exposes (refs/vte/src/vte.cc Terminal::get_text_displayed_a11y):
only the visible rows, soft-wrapped rows joined without '\n', so a '\n'-line is
a logical line except the top one, which can be the tail of a wrapped line.

The tracker sees ONLY successive snapshots of that text (worst case: no
text-changed offsets, e.g. VTE's full-refresh path when a frame scrolls >= one
screen, or a resync after lost events). It keeps a transcript of entries with
stable ids and aligns every new snapshot against the transcript tail by
line-hash voting. A review anchor is an entry id.

Metrics
- misalign: a snapshot line mapped to an entry created from a different true line.
- anchor jumps: the anchored entry now denotes a different true line (bad), vs
  baseline row anchoring (today's model: the cursor keeps its screen row).
- gaps: frames where lines scrolled through unseen; declared vs true.

Usage: python tools/research/term_anchor_sim.py [--frames N] [--seed S]
"""
from __future__ import annotations

import argparse
import random
import re
import time
from collections import Counter, defaultdict
from dataclasses import dataclass, field


# ----------------------------------------------------------------------------
# Ground-truth terminal
# ----------------------------------------------------------------------------
@dataclass
class TLine:
    tid: int
    text: str


class Terminal:
    def __init__(self, cols: int, rows: int):
        self.cols, self.rows = cols, rows
        self.lines: list[TLine] = [TLine(0, "")]  # last line = cursor line
        self.next_tid = 1
        self.floor = 0  # index of first logical line visible after a clear

    def newline(self, text: str = "") -> None:
        self.lines.append(TLine(self.next_tid, text))
        self.next_tid += 1

    def write_line(self, text: str) -> None:
        """Print text then '\n' (text goes onto the cursor line)."""
        self.lines[-1].text += text
        self.newline()

    def rewrite_cursor_line(self, text: str) -> None:
        """'\r' + text + erase-to-eol: in-place rewrite (progress bars)."""
        self.lines[-1].text = text

    def clear(self) -> None:
        """Screen cleared; VTE scrolls it into history, viewport starts empty."""
        self.floor = len(self.lines) - 1

    def wrap(self, text: str) -> list[str]:
        if not text:
            return [""]
        return [text[i:i + self.cols] for i in range(0, len(text), self.cols)]

    def snapshot(self) -> tuple[str, list[int]]:
        """Accessible text of the viewport and the true id of each '\n'-line."""
        rows: list[tuple[int, str, bool]] = []  # (tid, chunk, soft_wrapped)
        for ln in reversed(self.lines[self.floor:]):
            chunks = self.wrap(ln.text)
            for k in range(len(chunks) - 1, -1, -1):
                rows.append((ln.tid, chunks[k], k < len(chunks) - 1))
            if len(rows) >= self.rows:
                break
        rows = rows[: self.rows][::-1]
        out, tids, cur = [], [], None
        for i, (tid, chunk, soft) in enumerate(rows):
            if cur is None:
                cur = [chunk]
                tids.append(tid)
            else:
                cur.append(chunk)
            if not soft or i == len(rows) - 1:
                out.append("".join(cur))
                cur = None
        return "\n".join(out), tids


# ----------------------------------------------------------------------------
# Tracker (the algorithm under test)
# ----------------------------------------------------------------------------
def norm(s: str) -> str:
    return s.rstrip()


def h64(s: str) -> int:
    return hash(s) & 0xFFFF_FFFF_FFFF_FFFF


@dataclass
class Entry:
    eid: int
    text: str
    h: int
    version: int = 0
    partial: bool = False
    gap: bool = False
    truth: int = -1  # evaluation only: true id seen when the entry was created


@dataclass
class Tracker:
    entries: list[Entry] = field(default_factory=list)
    live: int = 0  # index of first entry of the current screen
    next_eid: int = 1
    gaps: int = 0
    misalign: int = 0
    inplace: int = 0
    removed: list[int] = field(default_factory=list)
    lookback: int = 64  # extra history lines allowed to re-enter the screen
    min_match: int = 2  # exact non-blank matches needed to accept an overlap
    min_frac: float = 0.6  # matches / (matches + mismatches) over the overlap

    def _new(self, text: str, truth: int) -> Entry:
        e = Entry(self.next_eid, text, h64(text), truth=truth)
        self.next_eid += 1
        return e

    def align(self, new: list[str], hint: int | None = None) -> int | None:
        """Start of `new` in transcript coordinates, or None (no overlap)."""
        w0 = max(0, self.live - self.lookback)
        window = self.entries[w0:]
        occ: dict[int, list[int]] = defaultdict(list)
        for i, e in enumerate(window):
            if not e.gap and e.text:
                occ[e.h].append(i)
        votes: Counter[int] = Counter()
        for j, line in enumerate(new):
            if not line:  # blank lines carry no information
                continue
            pos = occ.get(h64(line))
            if not pos:
                continue
            wgt = 1.0 / len(pos)  # IDF-like: repeated lines vote weakly
            for i in pos:
                votes[w0 + i - j] += wgt
        # The no-scroll hypothesis is always a candidate (covers screens with no
        # informative line, e.g. right after a clear); so is the event hint.
        cands = set(votes) | {self.live}
        if hint is not None:
            cands.add(hint)
        # Verify each voted shift over the whole overlap; votes only nominate.
        best_s, best_key = None, None
        n_old = len(self.entries)
        for s in cands:
            if s < w0 or s + len(new) < self.live:
                continue  # would delete history below the new screen: impossible in VTE
            if any(e.text for e in self.entries[s + len(new):]):
                continue  # text never vanishes below the screen in a scrolling surface
            ov = min(len(new), n_old - s)
            match = mism = nb = 0
            for j in range(0, ov):
                e, line = self.entries[s + j], new[j]
                nb += 1 if e.text else 0
                if e.gap:
                    mism += 1
                elif e.text == line:
                    match += 1 if line else 0
                elif j == 0 and line and e.text.endswith(line):
                    match += 1  # top line is the visible tail of a wrapped line
                elif line and e.text:
                    mism += 1  # blank -> text is the cursor line filling, not a conflict
            need = min(self.min_match, nb)  # a sparse old screen cannot offer more
            if match < need or match < self.min_frac * (match + mism):
                continue
            if need < self.min_match and mism:
                continue  # relaxed acceptance only for a perfect overlap
            key = (s == hint, match - mism, -(s - self.live) if s >= self.live else -10**9 - s)
            if best_key is None or key > best_key:
                best_s, best_key = s, key
        return best_s

    def update(self, text: str, tids: list[int], hint: int | None = None,
               force_gap: bool = False) -> None:
        new = [norm(x) for x in text.split("\n")]
        s = None if force_gap else self.align(new, hint)
        if s is None:
            if self.entries:
                g = self._new("", -1)
                g.gap = True
                self.entries.append(g)
                self.gaps += 1
            self.live = len(self.entries)
            for j, line in enumerate(new):
                e = self._new(line, tids[j])
                e.partial = j == 0
                self.entries.append(e)
            return
        # Map new[j] -> entries[s + j]; s may be < 0 only if lookback is exceeded.
        s = max(s, 0)
        out: list[Entry] = self.entries[:s]
        tail = self.entries[s:]
        for j, line in enumerate(new):
            if j < len(tail):
                e = tail[j]
                if e.gap:
                    e = self._new(line, tids[j])
                elif e.text == line:
                    pass
                elif j == 0 and e.text.endswith(line):
                    pass  # partial top line: keep the fuller text
                elif j == 0 and line.endswith(e.text) and e.partial:
                    e.text, e.h = line, h64(line)  # we now see more of it
                else:
                    e.text, e.h, e.version = line, h64(line), e.version + 1
                    self.inplace += 1
                if e.truth != tids[j]:
                    self.misalign += 1
                out.append(e)
            else:
                out.append(self._new(line, tids[j]))
        # Entries below the new screen bottom vanished (clear / redraw): drop them.
        for e in tail[len(new):]:
            self.removed.append(e.eid)
        self.entries = out
        self.live = s

    def index_of(self, eid: int) -> int | None:
        # Live region scan: O(rows); history ids would be O(1) via a ring index.
        for i in range(len(self.entries) - 1, -1, -1):
            if self.entries[i].eid == eid:
                return i
        return None


# ----------------------------------------------------------------------------
# Workload
# ----------------------------------------------------------------------------
WORDS = "build fetch link compile resolve worker request cache index shard commit sync".split()


def log_line(rng: random.Random, n: int, mode: str) -> str:
    if mode == "lowentropy":
        return rng.choice([".", "ok", "", "ok", "PASS"])
    r = rng.random()
    if r < 0.05:
        return ""
    if r < 0.10:
        return "error: " + " ".join(rng.choices(WORDS, k=rng.randint(2, 8)))
    if r < 0.15:
        return "warning: deprecated " + rng.choice(WORDS)
    if r < 0.22:
        return "    at module." + rng.choice(WORDS) + "()"  # repeated-ish stack lines
    if r < 0.27:
        return "x" * rng.randint(150, 400)  # long line, wraps
    ts = f"2026-10-17T12:{(n // 60) % 60:02d}:{n % 60:02d}.{rng.randint(0, 999):03d}"
    return f"{ts} INFO {rng.choice(WORDS)} " + " ".join(rng.choices(WORDS, k=rng.randint(1, 12)))


def run(frames: int, seed: int, mode: str, flood: float, hints: bool = False) -> dict:
    rng = random.Random(seed)
    term = Terminal(cols=100, rows=40)
    tr = Tracker()
    n = 0
    anchor_eid = None
    anchor_truth = None
    row_anchor = None
    row_truth = None
    jumps = row_jumps = anchor_lost = 0
    true_gaps = resizes = clears = 0
    last_seen_tid = 0
    t_align = 0.0
    prev_tids = None
    gap_by: Counter[str] = Counter()
    mis_by: Counter[str] = Counter()
    for f in range(frames):
        # operations between two snapshots
        if rng.random() < flood:
            k = rng.randint(20, 400)  # flood frame (VTE full-refresh path when >= rows)
        else:
            k = rng.choice([0, 1, 1, 1, 2, 3])
        for _ in range(k):
            n += 1
            if rng.random() < 0.08:
                for p in range(rng.randint(2, 6)):
                    term.rewrite_cursor_line(f"downloading [{'#' * p}{'.' * (10 - p)}] {p * 10}%")
                term.write_line("")
            else:
                term.write_line(log_line(rng, n, mode))
        cause = "flood" if k >= 20 else "normal"
        if rng.random() < 0.01:
            cause = "resize"
            term.cols = rng.randint(40, 200)  # reflow
            term.rows = rng.randint(15, 60)
            resizes += 1
        if rng.random() < 0.003:
            term.clear()
            cause = "clear"
            clears += 1
        text, tids = term.snapshot()
        # did lines scroll through unseen?
        first = tids[0]
        if first > last_seen_tid + 1 and last_seen_tid:
            true_gaps += 1
        last_seen_tid = tids[-1]
        # VTE's scroll path (scrolled < rows, no resize/clear) emits delete(0, k): the
        # number of logical lines that left the top is exact. Emulate that hint.
        hint = None
        if hints and cause == "normal":
            for i in range(len(tr.entries) - 1, max(-1, len(tr.entries) - 200), -1):
                if tr.entries[i].truth == tids[0]:
                    hint = i
                    break
        # VTE's scroll >= row_count path emits delete(0, all) + insert(0, all)
        # (refs/vte/src/vteaccess.cc:_vte_terminal_accessible_text_scrolled): no old row survives.
        force_gap = hints and prev_tids is not None and not (set(tids) & prev_tids)
        prev_tids = set(tids)
        g0, m0 = tr.gaps, tr.misalign
        t0 = time.perf_counter()
        tr.update(text, tids, hint, force_gap)
        t_align += time.perf_counter() - t0
        if tr.gaps > g0:
            gap_by[cause] += 1
        if tr.misalign > m0:
            mis_by[cause] += 1
        lines = text.split("\n")
        # anchors: (re)place on a random non-blank visible line every 25 frames
        if anchor_eid is None or f % 25 == 0:
            cand = [j for j, l in enumerate(lines) if l.strip()]
            if cand:
                j = rng.choice(cand)
                anchor_eid = tr.entries[tr.live + j].eid
                anchor_truth = tids[j]
                row_anchor, row_truth = j, tids[j]
            continue
        i = tr.index_of(anchor_eid)
        if i is None:
            anchor_lost += 1
            anchor_eid = None
        elif tr.entries[i].truth != anchor_truth:
            jumps += 1
            anchor_truth = tr.entries[i].truth
        if row_anchor is not None:
            r = min(row_anchor, len(tids) - 1)
            if tids[r] != row_truth:
                row_jumps += 1
                row_truth = tids[r]
    return dict(mode=mode, flood=flood, hints=hints, frames=frames, misalign=tr.misalign, inplace=tr.inplace,
                anchor_jumps=jumps, anchor_lost=anchor_lost, row_anchor_jumps=row_jumps,
                gaps_declared=tr.gaps, true_gaps=true_gaps, resizes=resizes, clears=clears,
                align_us_per_frame=round(1e6 * t_align / frames, 1),
                transcript=len(tr.entries), gap_frames_by_cause=dict(gap_by),
                misalign_frames_by_cause=dict(mis_by),
                duplicate_entries=sum(c - 1 for t, c in Counter(e.truth for e in tr.entries if e.truth >= 0).items() if c > 1))


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--frames", type=int, default=20000)
    ap.add_argument("--seed", type=int, default=1)
    a = ap.parse_args()
    for hints in (False, True):
        for mode, flood in [("log", 0.0), ("log", 0.05), ("log", 0.3), ("lowentropy", 0.0), ("lowentropy", 0.05)]:
            print(run(a.frames, a.seed, mode, flood, hints), flush=True)


if __name__ == "__main__":
    main()
