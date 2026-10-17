# M9 review-fix e2e gate triage (m9r .. m9r4)

2026-10-17, reviewer. Branch worktree `.claude/worktrees/agent-a5bd784f7a1f0f564`, head `942af2fc`. Read-only triage of
`bench/results/e2e/m9r*` on the branch against `main` baselines in `bench/results/e2e` of the main checkout. No new runs
(host load 12-13 with the implementer's matrix still running; the existing data already separates the classes).

## 1. Key fact: m9r3 and m9r4 ran the same code

`git diff --stat eff0ad4c 942af2fc -- crates tests` is empty: every commit after `eff0ad4c` only adds results. m9r3
(`eff0ad4c`) and m9r4 (`82329a7e`, `568474e0`, `bfbb3295`) are the same reader and the same harness. Any gate that fails in
one and passes in the other is not caused by code.

Every m9r4 failure is a `max over repeats` gate with **median 0** (FS2.page_dropped in seq-cr: median 6, see below):
one bad repeat out of 3 or 5.

## 2. Gate by gate

| Run | Gate (median/min/max) | m9r3 same code | main baseline (same shape) | Class |
|---|---|---|---|---|
| cr-typing | FS2.page_dropped 0/0/7 | 0/0/6 FAIL | FF `m9-ff-typing` (ba02adc) 8, `m8-cmp-main-typing` (7697d7f) 9/0/14 | (b)+(c)+(d) |
| seq-cr | FS2.page_dropped 6/0/18 | n/a | as above | (b)+(c)+(d) |
| seq-ff | FS2.page_dropped 2/0/4 | n/a | as above | (b)+(c)+(d) |
| cr-typing | FC1.lost 0/0/6, FC1.fc_double 0/0/6, FC1.fc_late 0/0/39 (fc1_ms p95 max 733 ms in one repeat) | 0/0/0, p99 max 8.7 ms | `m8-final-m7` FF: lost 2, double 2, late 104; `m8-rr6-fc` (a456491) FF: lost 90, double 16, late 142 | (b)+(c)+(d) |
| ff-typing | FC1.lost 0/0/1 | 0/0/0 | as above | (b)+(c)+(d) |
| cr-typing | FC1h.fc_extra 0/0/1 (`"a"` at +101.4 ms) | 0 | `m8-rr6-fc` FC1h.fc_extra 1 | (b)+(c) |
| ff-typing | FC1h.fc_double 0/0/1 (stray `"b"` at +101.5 ms) | ff m9r3: double 1, extra 2, late 1, page_dropped 18 | as above | (b)+(c) |
| ff-typing, seq-cr | FS2.fc_extra 0/0/1 (bare `"q"` at +12.9 / +19.5 ms) | 0 | `m9-ff-typing` FS2.fc_extra 1; `m8-cmp-main-typing` 0/0/2; same bare-letter notes `"n"`, `"q"`, `"p"`, `"o"` | (c) |

No (a). No gate in sections 2 or 3 points at a commit in `main..HEAD`.

### 2.1 Why each one is what it is

* **page_dropped** counts keys after which *the page* emitted no caret event (`webtext.rs` FC1h ~l.680, FS2 ~l.1203).
  It is a measurement of the browser under load, not of gorca; gorca's speech after those keys is already judged by
  `fc_extra`. Main fails it on Firefox at the same rates. Its gate (max over repeats <= 2) makes the run outcome depend
  on host load. Class (d) for the gate, (b) for the cause.
* **FC1 lost/double/late**: the m9r4 Chromium repeat shows keys #204-#207 each answered by `"Line 0204"` at
  +100-106 ms, then the page catches up (`m9r4-cr-typing/20261017T135518Z.txt` notes 4-13). +100 ms is
  `Cfg::edge` (`crates/gorca-core/src/text/caret.rs:80`): with no caret event 100 ms after a Down, CR-3 speaks the
  current line as a boundary (`caret.rs:1430-1446`). The page stalled ~0.5 s; gorca repeated the old line. `caret.rs`
  is unchanged in `main...HEAD`. FC1, unlike FC1h and FS2, scores against a model (`Line {i}`), so every key the page
  delays or coalesces becomes `lost` + `fc_double` + `fc_late` (the 733 ms "latency" is the event-to-key misalignment
  of the same stall, not reader time). Firefox `lost #126` in ff-typing is a coalesced pair (heard `Line 0128` at +137 ms).
* **FC1h stray at +101 ms**: same CR-3 edge timer firing before a late (not dropped) caret event. Pre-existing.
* **FS2 bare letter**: a caret event in a Shift+arrow run spoken as a plain move (no "selected"/"unselected").
  Identical notes on main Firefox. Pre-existing text-layer behaviour; worth a ticket, not an M9 item.

## 3. Earlier rounds, for completeness

| Round | Failure | Status |
|---|---|---|
| m9r (4555f3c), m9r2 (a64b618) | BAR1.bar_failures 1/1/1 | fixed by `a64b618`+`cec529d5`; passes m9r3, m9r4 (real, closed) |
| m9r, m9r3 | FE2.e2_resync max 6 | page did not apply actions (load); m9r4 0 (b) |
| m9r2 cr-m8 | M8-QN.fail 0/0/2, M8-FORGE.fail 1 | not in m9r3/m9r4 cr-m8 (b) |
| m9r3 ff-m7 | W2b gorca p99 max 3003 ms, untagged_warm 1 | load; W2b already failed on main at load (`m9-baseline.md` 3.2); m9r4 ff-m7 pass (b)+(c) |
| m9r3 ff-m8 | M8-LV.live_ms.p99 101 ms | m9r4 pending (ff-m8 running at triage time) (b) |
| m9r seq-ff | FCE1 lost/late/extra | the H-1 fix target; FCE1 is 0 in every m9r4 run incl. seq-cr/seq-ff x5 (fix verified) |

## 4. Verdict

The matrix is converged in the only sense it can be on this host: across two full rounds of identical code, no gate
fails with a non-zero median except page_dropped, which measures the browser, and every failing max has the same
shape on `main`. Rerunning the matrix cannot turn these green; it only samples the load.

Shortest path to merge-ready:
1. Stop rerunning. Let the running ff-m8 finish; if it has no FAIL with a non-zero median, the e2e evidence is complete
   (m9r3 + m9r4 together).
2. Record in `m9-impl.md` section 5 as known issues, pre-existing on main (with the run paths above):
   K-1 page_dropped is browser-side; K-2 CR-3 edge speech (`Cfg::edge` 100 ms) repeats the old line while the page
   stalls (FC1/FC1h strays and doubles); K-3 FS2 bare-character stray.
3. Gate re-review only needs: `cargo fmt`, clippy `-D warnings`, `cargo test --workspace` at `942af2fc`.

Follow-ups, not blocking M9 (harness, test-engineer):
* FC1: derive expectations from the page's caret events like FC1h/FS2 and count stalled keys in `page_dropped`.
* `page_dropped`: report as information, or gate the median, not the max.
* Gate `fc_*` on the median with a per-run max recorded, or rerun only the failing scenario once before failing.
Product follow-up (text layer): K-2 is audible on a hung page (the same line repeated on each Down); consider
suppressing the CR-3 boundary speech while an earlier key of the same tracked object still has no caret event.
