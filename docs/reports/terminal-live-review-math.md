# Terminal live review: the math (anchoring, incremental diff, flood speech)

Date: 2026-10-17. Author role: math-researcher. Companion to `docs/reports/terminal-live-review.md` and
ADR-0018. This report specifies the algorithmic core behind ADR-0018 Option A ("bounded shift
estimate") and Option C (`ext-vte` continuity inference that feeds `Committed{first, lines, gap_before}`,
plus the core `LineLog` and `TermPos{epoch,row,col,hash}`).

Prototypes (project venv, pure Python, no cargo):
- `tools/research/term_anchor_sim.py`: ground-truth VTE model plus the tracker below; measures anchor
  stability, misalignment and gap detection.
- `tools/research/term_flood_policy.py`: queueing simulation of speech policies under floods.

Proven results and heuristics are kept apart: **[P]** means it follows from the model, **[H]** means
heuristic, validated only on synthetic data.

## 0. What VTE actually gives us (read from `refs/vte`)

- The accessible text is the **visible rows only**: `Terminal::get_text_displayed_a11y` reads from
  `scroll_delta` for `m_row_count` rows (`refs/vte/src/vte.cc:6906`). Scrollback cannot be read via AT-SPI.
- **Soft-wrapped rows are joined without `'\n'`** (`vte.cc`, the end of `get_text`: a newline is added
  only `if (!is_soft_wrapped(row))`). So a `'\n'`-separated line is a **logical line**. The exception is
  the top line, which can be the tail of a wrapped line whose head is above the viewport. **Reflow on
  resize does not change logical lines**; it only changes which part of the top line is visible.
- Scroll by `0 < howmuch < row_count` (`refs/vte/src/vteaccess.cc:_vte_terminal_accessible_text_scrolled`):
  `delete(0, drop)` for the rows that left the top, then `insert` at the bottom, plus an optional middle
  delete and insert. The number of logical lines that left is the count of `'\n'` in the deleted prefix,
  so it is **exact**.
- Scroll by `|howmuch| >= row_count`: `delete(0, len)` followed by `insert(0, len)` of the whole screen.
  **No old row survives.** This is the flood signature: VTE processes input in chunks, so a fast flood
  scrolls more than one screen per a11y update.
- Other content changes: a common-prefix/common-suffix diff (`vteaccess.cc`, around lines 400-460), at
  most one `delete` and one `insert`. Offsets are counted in characters (code points).

Consequence: on the event path, continuity is exact for small scrolls and is known to be broken for
large ones. Content alignment (§1.3) is needed only for: (a) resync after lost or unprotected events
(the `Replaced` re-read), (b) a new tracking (each Alt+Tab creates a new extension, see
`terminal-review-stale.md`), (c) resize, and (d) the generic prefix/suffix path when it mixes a scroll
with edits.

## 1. Position anchoring

### 1.1 Model

- **Transcript** `L`: a sequence of entries indexed by an absolute row `r ∈ ℕ` (u64, never reused).
  Each entry is `Line{text, h = H(norm(text)), version, partial}` or `Gap{kind: Unknown|Known(n)|Cleared}`.
  `norm` strips trailing whitespace. `H` is a 64-bit hash (xxh3 in Rust).
- **Floor** `f`: rows `< f` are evicted (ring buffer with a byte and line cap; ADR-0018 says 20k lines
  and 16 MiB). Access to row `r` is `ring[(r - f + head) mod cap]`, so it costs O(1).
- **Live window** `[v, |L|)`: the rows that are currently on screen. Rows `< v` are **committed** and
  immutable. Rows `>= v` may change in place (version++) and can be appended.
- **Epoch** `e`: increments only when continuity cannot be established *and* the old log is
  discarded. With the policy below this happens only on extension reset or alternate screen. A
  gap does not start a new epoch; it is an entry.

Screen-to-log mapping is **positional after a shift**: a new screen `N[0..m)` is placed at
`r = s + j`. Lines are never inserted into the middle of the log. A row's index is therefore fixed
from the moment it is created **[P]**. That is why `TermPos.row` can be absolute.

### 1.2 Event path (exact; this is the common case)

| VTE emission | Log action |
|---|---|
| `delete(0,k)` with `q = #'\n' in old[0..k]`, then bottom `insert` | `v += q` (commit q rows); the inserted text updates the last live row and appends rows |
| `delete(0,len)` + `insert(0,len)` (scroll ≥ rows) | commit all live rows; append `Gap{Unknown}` (or `Known(n)` when the scrollbar `Value` probe P-T1 gives `n`); append the new screen |
| prefix/suffix `delete(o,a)`/`insert(o,b)` inside the live window | edit the affected rows in place (version++); a `'\n'` change splits or merges only rows at or after the edit (append semantics, see §2) |

### 1.3 Content alignment (resync path)

Input: the live log tail `W = L[w0..|L|)`, with `w0 = max(f, v - B)` and look-back `B = 64` (rows can
come back on a resize that grows the window, or on the user's scrollback), plus the new screen `N`.

1. **Nominate** (hash voting). For each non-blank `N[j]` and each `i` with `L[i].h = H(N[j])`, add
   `1/|occ(h)|` to `votes[i - j]`. The always-added candidates are `v` (no scroll) and the event hint
   when there is one.
2. **Verify** each candidate `s`. Overlap is `j ∈ [0, min(m, |L|-s))`.
   - `match`: count of exact non-blank equalities, plus `j = 0` when `L[s].text` ends with `N[0]` (the
     partial top line).
   - `mism`: both lines non-blank and different. Blank → text is the cursor line filling up, so it is
     not counted.
   - Reject `s` if `s < w0`, or if `s + m < v` (that would un-commit history), or if any **non-blank**
     row lies below the new screen bottom (text never disappears below the screen of a scrolling
     surface).
   - Accept only if `match >= min(2, nb)` (`nb` = non-blank old rows in the overlap) and
     `match >= 0.6 (match + mism)`. When `nb < 2`, also require `mism = 0`.
3. **Choose** the candidate with the largest key: `(s == hint, match - mism, forward preference)`.
   Forward preference means the smallest `s >= v`; otherwise the largest `s < v`.
4. **Apply**: `v := s`; for each `j` in the overlap, an equal line keeps its row, a suffix match keeps
   the fuller text, a partial line that is now visible in full gets its text extended, and any other
   line is rewritten in place (version++). Rows beyond `|L|` are appended. Trailing blank live rows
   below the screen are dropped.
5. **No candidate accepted**: commit all live rows, append `Gap{Unknown}` (or `Cleared` when the
   screen is not full), then append `N`.

Complexity: O(m + Σ_j |occ(N[j])|) for voting and O(|cands| · m) for verification. In the worst case
(all lines identical) that is O(m·(m+B)). With m ≤ 200 and B = 64 this is at most about 5·10⁴ hash
compares, roughly 20-50 µs in Rust. Typical cost is O(m). It runs **once per coalesced update**, not
per event.

### 1.4 Anchor and resolution function

`Anchor = TermPos{epoch, row, col, hash}`, where `col` is a grapheme index in the logical line.

```
resolve(a, L) ->
  if a.epoch != L.epoch:            refind(a) else Lost{EpochBreak}
  if a.row < L.floor:               (L.floor, 0, Trimmed)           # announce once
  e = L[a.row]
  if e is Gap:                      (next Line row, 0, Gap)          # cannot happen for a row the user read
  if e.h == a.hash:                 (a.row, a.col, Exact)
  else:                             (a.row, min(a.col, len(e)-1), Rewritten)   # \r progress, cursor line
refind(a): align the current screen against the stored fingerprint window
  (h(row-2..row+2)); accept a unique exact 5-gram, else a unique line hash, else Lost.
```

Guarantees:
- **G1 no jump [P]**: if `a.row >= floor` and the epoch is unchanged, the row is returned unchanged.
  Rows are immutable indices (§1.1), so the cursor's line never changes because of output, scrolling,
  reflow or alignment. The only exceptions are eviction (`Trimmed`) and epoch break, and both are
  reported.
- **G2 order [P]**: `resolve` is monotone in `row`, so marks keep their relative order.
- **G3 column [P]**: `col` changes only by the clamp, and only when the line's hash changed.
- **G4 cost [P]**: O(1) (ring index plus a hash compare). `refind` is O(m) and runs only after an
  epoch break.
- **G5 content correctness [H]**: G1 says the *row* is stable. Whether the row still holds the *same
  text the user read* depends on the alignment being right, and that is a heuristic. §5 gives the
  measured error rates.

Failure cases (all reported, never silent): eviction; epoch break; `Gap` (a flood outran VTE); low
entropy (identical lines, `.`/`ok` loops, blank screens), where a shift is unidentifiable from content
alone. In that case the event hint, or the scrollbar `Value`, is required, and alignment otherwise
declares a gap. A line of the live window that is rewritten into different content (e.g. multi-line
redraws by `docker pull`) keeps its row, gets `Rewritten`, and its column is clamped.

**Grid surfaces** (alternate screen: vim, htop, less) are not logs. There, keep today's row anchoring
(`TermSurface::Grid`). Detection when VTE does not expose the alternate screen **[H]**: over the last
32 updates, the fraction explained by "scroll + edit of the last 2 rows" is below 0.5 while
the screen stays full → Grid. Hysteresis: return to Scrolling after 16 consecutive explained updates.

## 2. Incremental screen model and offset mapping

The structure is `Vec<Row{text: String, chars: u32, bytes: u32}>` for the live window (≤ rows, ≤ about
200 lines) plus a lazily rebuilt prefix-sum array `P[i] = Σ_{k<i}(chars_k + 1)`.

- `locate(offset)`: binary search on `P`, O(log R). Then the byte offset in the row is found by a
  char-to-byte walk over that row only, O(cols), or O(1) for ASCII rows, which carry a flag.
- `delete(o,n)` / `insert(o,t)`: locate both ends, splice at most two rows, and split or merge on
  `'\n'`. Cost O(R + |t|). `P` is marked dirty and rebuilt in O(R) on the next `locate`.
- `delete(0,k)` whole rows: `drain(..q)` into the committed log. Use `VecDeque` so this is O(q).

Options considered: a rope (`ropey`, O(log n) everything) is overkill when n ≤ 200 rows and is slower
in constant terms. A Fenwick tree over row lengths gives O(log R) point updates, but row
insert/delete forces an O(R) rebuild anyway. A piece table helps undo, which is not needed here.
**Chosen: VecDeque<Row> + lazy prefix sums.**

Per-event cost: at most about 1 µs for typical events (estimate, not measured in Rust). With 10⁴
events/s the actor load is ρ ≈ 0.01. M/D/1 waiting is `ρ·S/(2(1-ρ))`, which is negligible. The tail risk
is one huge insert (a full 200×500 screen is 100 k chars ≈ 50 µs). Content alignment runs at most once
per update batch: coalesce under a dirty flag and flush on idle or at 16 ms.

## 3. Flood summarisation for speech

### 3.1 Why FIFO is wrong [P]

With speech capacity `μ = μ_c / min(L̄, c_max)` lines/s (`μ_c` = 15-35 chars/s for 180-420 wpm) and
arrival rate `λ`, a FIFO is an M/D/1 queue with mean wait `ρD/(2(1-ρ))`. That wait is 4.5·D at ρ = 0.9
and unbounded for ρ ≥ 1. For a 60-char line at 30 chars/s, `μ` is 0.5 lines/s. So even a "chatty"
3 lines/s is overload, and floods are 10³-10⁵ times capacity. Measured (§5.2): FIFO staleness p50
reaches 1.3·10⁶ s.

### 3.2 Policy: pull scheduler + token bucket + summaries

The scheduler holds a single slot ("latest wins"). There is never a speech queue. It is driven by
the speech channel's idle callback (voicego and SSIP index-mark/end events). If those are missing,
use `chars/μ_c` as the estimated duration.

```
state: spoken_upto (row), tokens (≤ Bk), flood: bool, last_summary
on idle or new rows while idle:
  refill tokens at r = 0.7·μ (cap Bk = 3 lines)
  p = rows in (spoken_upto, tail]
  if flood and tokens == Bk and p ≤ 1: flood = false
  if !flood and p ≤ tokens: speak the p lines (each capped at c_max = 120 chars); tokens -= p
  else: flood = true; if now - last_summary ≥ g (2 s):
        speak "<p> new lines, last: <best line>"; spoken_upto = tail
```

`<best line>` is the newest "interesting" line (§4) if one arrived in the window, otherwise the newest
line, with run-length dedupe applied (§3.3).

**Staleness bound [P]**: a row that arrives at time `a` is conveyed by the first utterance that starts
after `a`. That start is at most `D_max = c_max/μ_c` (the rest of the current utterance) plus `g` (the
summary spacing). So staleness ≤ `g + c_max/μ_c`, which is 6 s at 30 c/s and 10 s at 15 c/s,
**independent of λ**. Measured max: 3.4-4.0 s at 30 c/s and 5.3 s at 15 c/s.

**Keypress latency**: review and keyboard speech have preemptive priority. They cancel the current
utterance and freeze auto-speech (ADR-0018 `Frozen`). They never queue behind output, so flood load
enters the 15 ms budget only through CPU (§2), and there ρ ≈ 0.01.

Parameters: `Bk = 3`, `r = 0.7·μ`, `c_max = 120`, `g = 2 s`. `μ_c` comes from the current voice rate.
The default is 25 c/s, recalibrated by EWMA of `chars / measured utterance time` (τ = 30 s). Varying
`enter` (r/μ) between 0.5 and 0.9 changed verbatim coverage by only ±10% (§5.2). The gate must be a token
bucket: a single-event EWMA rate estimator falsely triggered flood mode on one line, because
one arrival with τ = 1 s gives λ̂ = 1/s > μ. That design failed in the prototype and is documented here.

### 3.3 Dedupe and progress

- **Exact runs**: key `H(norm(line))`. Consecutive equal keys are collapsed into "<line>, repeated N
  times". Cost: O(1) per line.
- **Template runs** **[H]**: mask `[0-9]+`, `0x[0-9a-f]+` and UUIDs to `#`, then hash the result. Equal
  template keys give "N similar lines: <last>". Short cycles (A B A B): a ring of the last 8 template
  hashes; a hit means the cycle continues.
- **Progress** **[H]**: a live row with ≥ 3 in-place rewrites (version) within 2 s, or a row that
  matches `\d{1,3}(\.\d+)?%` or `[#=>\-.]{5,}`. Speak only when the percentage crosses a 10% step
  and at least 3 s have passed, plus the final text once the row stops changing for 1 s (or
  when it is committed). Spinner rows (only the first character changes) are never spoken.

## 4. Interesting lines (jump-to-next-interesting) [H]

- **Severity**: one compiled `RegexSet`, case-insensitive with word boundaries:
  `error|fail(ed|ure)?|fatal|panic|exception|traceback|warn(ing)?|denied|refused|segmentation fault|abort(ed)?|timed? ?out|\bE\d{3,}\b|exit (code|status) [1-9]`.
  Cost: O(len) per line, with a DFA shared across patterns.
- **Novelty**: Drain-lite template hash (as in §3.3). Keep a `HashMap<u64,(count,last_row)>` capped at
  8192 with LRU. A line is novel if its template was not seen in the last 10k rows. Use a 64-bit
  SimHash over word 2-shingles (Hamming ≤ 3, 4×16-bit band tables, expected O(1) lookup) only if
  template hashing proves too coarse on real logs.
- Store a per-row bit set `{severity, novel, prompt}` in the log. "Next interesting" is then a
  forward scan over bits (O(k/64) with a u64 bitmap) and needs no extra index.
- Not validated on real logs. Synthetic logs make template novelty perfect by construction, so no
  synthetic number is reported here.

## 5. Measurements

### 5.1 Anchoring (`term_anchor_sim.py`, 20k frames, 100×40 start; resize p = 0.01/frame to 40-200 cols × 15-60 rows; clear p = 0.003; 8% progress bars; 5% long wrapped lines; anchors re-placed every 25 frames ≈ 800 anchors)

"snap" means snapshots only (worst case). "evt" adds the two VTE event signatures (exact scroll hint for
`< rows`, forced gap for `>= rows`). Values are given as seed 1 / seed 2.

| workload | mode | misaligned line maps | anchor jumps | row-anchor jumps (today) | gaps declared / true |
|---|---|---|---|---|---|
| log, no flood | snap | 0 / 412 | 0 / 0 | 14224 / 14188 | 57 / 27 (clears ≈ 58 are counted as gaps) |
| log, no flood | evt | 0 / 0 | 0 / 0 | same | 57 / 27 |
| log, 5% flood frames | evt | 743 / 1145 | 0 / 1 | 14673 / 14667 | 1017 / 967 |
| log, 30% flood frames | evt | 385 / 691 | 1 / 0 | 16163 / 16247 | 5857 / 5782 |
| low entropy (`.`,`ok`), no flood | snap | 128 / 70 | 0 / 1 | 15309 / 15298 | 57 / 18 |
| low entropy, no flood | evt | 0 / 0 | 0 / 0 | same | |
| low entropy, 5% flood | snap | 46199 / 48554 | 60 / 41 | 15852 / 15843 | **290 / 955 (gaps missed)** |
| low entropy, 5% flood | evt | 1838 / 3620 | 4 / 3 | same | 1003 / 999 |

Each frame maps about 40 lines, so misaligned line maps are at most 0.15% of maps on the event path.
Anchor jumps are 0-1 per 20k frames, against 14-16k for today's row anchoring. Duplicated
entries (a line logged twice after a spurious gap): 5-21 per ~26k rows without floods. Python
alignment cost was 80-1200 µs per frame (higher in floods because of the transcript size and Python
overhead). Rust should be about 50× lower (estimate).

Findings that changed the design:
- Votes alone mis-aligned whole flood frames on repeated lines ("    at module.x()"). Overlap
  verification fixed this: before verification, flood misalign was 213k, after it was 691.
- Allowing a candidate to delete non-blank rows below the screen destroyed history after `clear`.
  The rule "text never vanishes below the screen" removed all clear-induced misalignments.
- Low-entropy floods cannot be aligned from content (an inherent limit). The `delete(0,all)+insert(0,all)`
  signature recovers gap detection (1003 declared / 999 true).

### 5.2 Flood speech (`term_flood_policy.py`, 1 h MMPP: quiet 0.3/s, chatty 3/s, flood 3000/s; 1.27 M lines, 4140 outside floods; 60-char lines)

| μ_c | policy | staleness p50 / p99 / max | verbatim lines | utterances | summaries |
|---|---|---|---|---|---|
| 30 c/s | FIFO | 1.27·10⁶ s / 2.5·10⁶ s / — | all, eventually | 1.27 M | 0 |
| 30 c/s | pull, enter = 0.7 | 0.12 s / 2.49 s / 3.39 s | 509 | 1174 | 704 |
| 15 c/s | pull, enter = 0.7 | 0.37 s / 4.64 s / 5.29 s | 214 | 669 | 484 |

Verbatim lines are capped by physics. At 30 c/s, 3 lines/s is 6× speech capacity, so most chatty
lines are necessarily summarised. Sanity case: 1 line every 5 s → all 50 verbatim, 0 summaries.

## 6. Test vectors (alignment; old screen = log rows 0.., `v = 0`; expected start `s`)

| case | old | new | s |
|---|---|---|---|
| scroll2 | a1 b2 c3 d4 "" | c3 d4 e5 f6 "" | 2 |
| progress in place | a1 b2 "dl 10%" | a1 b2 "dl 20%" | 0 (row 2 Rewritten) |
| partial top | xxxxxxxxxxyy b2 c3 "" | yy b2 c3 d4 "" | 0 |
| one-line overlap | a1 b2 c3 "" | c3 d4 e5 f6 | 2 |
| resize grow (history returns) | a1 b2 c3 d4 "" | b2 c3 d4 "" | 1 |
| no overlap | a1 b2 c3 "" | p1 q2 r3 "" | none → Gap |
| clear | a1 b2 c3 "" | "$ " | none → Cleared |
| periodic | ok ok ok ok "" | ok ok ok ok "" | 0 (ambiguous; snap picks no-scroll) |

## 7. Porting notes (Rust)

- `LineLog`: a `VecDeque<Entry>` ring with `floor: u64`. `Entry` is `{text: Arc<str>, h: u64, version: u32,
  flags: u8}`. A byte budget evicts from the front. Never use `unwrap`: index with `get(r - floor)`.
- `xxh3_64` over `norm(text)`. Equal hashes are confirmed by a string compare on verify, because a
  64-bit collision across 10⁵ rows has probability about 3·10⁻¹⁰, but the check costs nothing.
- Voting uses a `HashMap<u64, SmallVec<[u32;2]>>` built over the window once per alignment, or a
  sort-merge of `(h, i)` pairs, which avoids allocation.
- Model alignment as a pure function `align(&LogTail, &[&str], hint: Option<u64>) -> Option<u64>`
  so it can be property-tested. Invariants: G1 (rows of entries `< v` are bit-identical before and
  after), the absence of a non-blank delete below the screen, and an idempotent re-apply of the
  same screen (`s = v`, no version change).
- The flood scheduler is an explicit `enum FloodState { Normal{tokens}, Flood{last_summary} }` driven by
  `SpeechIdle` and `RowsAppended` messages. It never blocks, and the time comes from the actor's
  clock.

## 8. Risks / open questions

- All numbers are synthetic. Real VTE batching, the `clear`/`reset`/Ctrl+L signatures (P-T2), and
  alternate-screen detection are unverified. Low-entropy logs without the event hint still miss gaps.
- Scrollbar `Value` (P-T1) would turn `Gap{Unknown}` into `Known(n)` (in rows, not logical lines:
  `n_lines ≤ n_rows`).
- In the 5% flood workload (event path), 743-1145 misaligned line maps remain unexplained, all in
  frames right after a flood. Anchors were unaffected (0-1 jumps), but the cause should be found
  before the port.
- Speech capacity `μ_c` per voice and rate is estimated, not measured with voicego.
- Interesting-line heuristics are unvalidated on real logs.
