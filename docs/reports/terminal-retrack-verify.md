# Terminal re-track fix: review (verify)

Date 2026-10-17. Role: reviewer. Branch `worktree-agent-a33489069c15d31be`, head 917cb9ab, base 10ab9f6f.
Subject: `docs/reports/terminal-retrack-fix.md`.

## Verdict

**MERGE AFTER FIXES.** Claims (1)-(3) hold. Claim (4), the `review-drift` change, is a valid fix for
the case it was written for: the line was rewritten, not lost. It also makes the oracle looser than
it needs to be (L1). One must-fix: a new strict-statemodel failure (M1) that the branch's session
churn causes. The product does not spin, but the long statemodel gate turns red.

## Gates (worktree, CARGO_BUILD_JOBS=4, RUST_TEST_THREADS=4, load avg 10-12)

| Gate | Result |
|---|---|
| `cargo fmt --all -- --check` | ok |
| `cargo clippy --workspace --all-targets -- -D warnings` | ok |
| `cargo test --workspace --no-fail-fast` | run 1: exit 101, and the failing test's name was cut off by `head`. Runs 2 and 3 were clean: 2180 passed, 0 failed, 17 ignored. I treat it as a load flake, but it is unverified. |
| `cargo xtask strings` | ok (318 literals, 102 allowed) |
| `git merge-tree --write-tree main HEAD` | clean. Since the base, main changed only `trackable()` (ComboBox), with no overlap. The merged tree was not built. |
| statemodel strict 50k, LEN 40, seed 777001 | clean |
| statemodel strict 20k, LEN 150, seed 9100000 | **FAIL: `panic` timer storm, first seed 9100776** (M1) |

## Findings (ranked)

### M1 (must fix) The cold re-track multiplies timer chains, and the strict statemodel run reports a timer storm

The failure is in `crates/gorca-core/src/terminal.rs` (cold path of `term_retrack_on_key`), seen through
`statemodel/world.rs:486` (the timer list).

Repro: `GORCA_STATE_STRICT=1 GORCA_STATE_CASES=1 GORCA_STATE_LEN=150 GORCA_STATE_SEED=9100776 cargo test --release -p gorca-core --lib statemodel::random_sequences`.
It reports "timer storm ... at 6275 ms" in a 26-step sequence: HoldReplies, FailFetches, terminal focus,
review commands and keys at 1 ms gaps.

| Configuration | Result |
|---|---|
| base 10ab9f6f | clean |
| branch with the core terminal changes reverted | clean |
| branch with the cold path disabled (env-guarded hack, now reverted) | clean |

Mechanism, from instrumented traces: while every read fails, each key re-tracks, which creates a new
session (18 sessions in 3.4 s). The deadlines that dead sessions armed stay in the timer list. Each one
fires a `Tick` into the *current* session. `on_tick` clears `armed`, so the current session re-arms its
deadline once per tick. The sim keeps timers in a `Vec`, so the same `Instant` was armed 44 times per
deadline, and 44 parallel chains ran for ever.

The production runtime keeps timers in a `BTreeSet<Instant>` (`runtime.rs:1046,1133`), which collapses
identical deadlines. With the same dedup added to the sim (hack, reverted), seed 9100776 and the whole
20k x 150 run are clean. So this is not a production busy loop. It is a fidelity gap that the new churn
exposes.

Fix:
- Make the sim's timers a set (same semantics as `runtime.rs`).
- Add the 26-step sequence as a regression.
- Optional: have a re-track not inherit the dead session's deadlines. For example, ignore a `Tick`
  whose deadline the current serial never armed.

### M2 (should fix) "Bounded by one read in flight" is false: one key can start two sessions and orphan a read

The cause is `text/mod.rs:1290` (`term_resume` requires `read_ok`) together with `term_retrack_on_key`.
When a key leaves review on a cold terminal, it re-tracks twice in the same millisecond:
- the key path (cold), giving serial N;
- then `text_mode_changed`, where `term_resume` is false because there is no answered read, so
  `text_track` runs and gives serial N+1.

Each new session drops the previous session's in-flight fetch: the reply is ignored, but the IPC is
still outstanding. The trace shows serials 2/3, 4/5, 7/8, 11/12 and 13/14 at identical timestamps. The
result is two reads per key to an application that is already failing.

Fix: in `text_mode_changed`, do not re-track a terminal that this turn already re-tracked or that has a
fetch in flight. Alternatively, let `term_resume` accept a real session with `fetch.is_some()`.

### L1 (low, oracle) The new `review-drift` exemption is self-referential and broader than the case it fixes

`statemodel/mod.rs:710` and `review/commands.rs:1465`.

The shrunk case (seed 2021185) is legitimate. The prompt line `me@box:~$ z` was *rewritten* to
`me@box:~$` on the same row. The cursor kept its column and advanced: it spoke "at", the next
character. ADR-0018 decision 1 allows "a clamp to a rewritten line". The old invariant was wrong here.

The new condition, however, accepts *any* destination whenever the old text is absent from the
review's own model (`review.cur`), which is the thing under test. A rewrite-plus-scroll drift, such as
`prog 10%` becoming `prog 20%` with a shift (P3 of the anchor verify), is no longer checked.

Tighten: when `t0` is gone, require `t1` to be on the row the old line occupied, carried by the shift,
or the held line. Read `t0` presence from the world buffer (`s.w.bufs[TERM]`), not from `review.cur`.

The regression name `review_by_character_stays_on_its_line_*` also contradicts its doc comment.

### What the user hears when the reviewed line vanishes

Reviewer probes (statemodel `trace`):
- **RP3**: the line vanished entirely (`flood 3`, then the whole screen was replaced by `[1]+ done` and
  the prompt), with events delivered. NextChar gives "o", "o" from the held line, CurLine gives
  "flood 3", and NextLine gives "scrolled, 1 plus done". This matches ADR-0018 decision 2 and the
  anchor fix. Correct.
- **RP1**: output during review, then review is turned off. Nothing is spoken at resume (no flood).
  Up then gives "cmd1". Correct.
- **RP4 (pre-existing, not this branch, medium)**: events were lost while the line vanished. NextChar
  speaks "l" from the stale `flood 3` first. The re-read replaces the screen, and **no correction is
  spoken**. The next NextChar says "at" from the prompt line, a silent jump. This contradicts H1 of the
  anchor fix ("row kept, new line corrected"). Neither the old nor the new invariant catches it,
  because the cursor moves during a settle step, not during a review command. `review/commands.rs` is
  unchanged by this branch. File it as an ADR-0018 follow-up: a statemodel check that "a speak-first
  line replaced by the re-read is corrected or announced".

### Checked and sound

- **Interval starvation:** `retrack_at` is set only after a real session opened (degraded/cold branch),
  or when `started && real`. A stand-in never consumes it, so a degraded terminal is upgraded at the
  first key after the slot is healthy.
- **Non-blocking (R3):** the key path only pushes effects. The cold predicate reaches
  `sess.snapshot()` (a WASM call) only after cheap short-circuits.
- **Secret hold:** the cold path shares the restore of the degraded path. `term_resume` keeps the
  session, so the secret watch survives leaving review. That is an improvement: the old path cleared
  `secret_hold` through `term_untrack`.
- **ext-vte `flush_vertical`:** the old code was a real production spin. `deadline()` tests the raw
  `expect`, while `flush_vertical` tests `expect_valid(now)`, so a due deadline was never consumed and
  `ArmTimer(d.max(now))` re-armed at now. Now fixed. Side effect (low): a history key whose read
  outlives `EXPECT_MS` is now silent instead of spinning.
- **History recall after review:** regression `history_recall_after_review_is_spoken` passes, and so
  does RP1.

### Open items: classification

- `not-tracked` (seed 5014211, LEN 150): reproduced on the branch, and also with the core terminal and
  ext-vte changes reverted to 10ab9f6f. The sequence is OpenDialog, reads fail, CloseDialog (refocus),
  OpenDialog, reads work, then `,` in the entry. It is a pre-existing entry/dialog tracking race and
  does not block this merge.
- VD round 2 "5 5 5 echo": identical in `retrack-VD-main` (main binaries). The text reached bash
  garbled ("bash: 5 5 5 echo: command not found"), so the cause is key injection or ordering in the rig
  during the tab switch, not reader output. Pre-existing; does not block.

### Low (docs)

`terminal-retrack-fix.md` says "No invariant was changed" in "Model check", but the follow-up section
changes `review-drift`. Reconcile the two.

## Must-fix list

1. M1: the sim's timer set uses runtime semantics, and the seed 9100776 sequence becomes a regression.
   Re-run strict LEN 150.
2. (Recommended, same round) M2: one re-track per key, with no orphaned read.
3. (Recommended) L1: tighten the `review-drift` exemption to the carried row or the held line, checked
   against world truth.
