# Re-review (round 2): B-1 adoption

Date: 2026-10-15. Branch `worktree-agent-a259b4477ef251616`, head `875ce66`. Round 1 was
`docs/reports/b1-adopt-review.md` at `df20f8c`. Main is now `6fc592b`.
Review tree: `.claude/worktrees/b1-rereview` (detached at 875ce66, plus probes). Evidence is in
`bench/results/b1-adopt-rereview/` in that tree.

**Verdict: MERGE AFTER FIXES.** F-1, F-2, F-4, F-5 and F-6 are closed. F-3 is only partly closed.
One fix adds a regression (RR-1): together, the RV-1 retry and the RV-1 after a Focus timeout can
make a slow but live app read as Hung. The fix is small. Everything else is ready.

## Gates (reviewer, on 875ce66)

| Gate | Result | Load (1-min) |
|---|---|---|
| `cargo fmt --all -- --check` | clean | 9.4 |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean | 9.4 |
| `cargo test --workspace --no-fail-fast` | 1452 passed, **1 failed**, 15 ignored | 9.4 -> 9.7 |

The failure is `gorca-testkit cpu::tests::spin_is_measured`. It is a load flake in a file the branch
does not touch: a 2 ms wall-clock spin measured less than 1 ms of CPU, and it passed 3 out of 3
times on rerun. Log: `gates.log`.

Trial merges were made in a scratch worktree and then removed. Conflicts were resolved as the
union of both sides, and for `pipeline.rs` main's side was taken.
* **Branch + main (6fc592b).** Conflicts in `pipeline.rs` (the hung test; main relaxed it the same
  way, with a stronger follow-up assertion, so take main's), `daemon/cli.rs` (`--shell-cache-dump`
  next to `--topology`; drop main's stale `--bench-fill-policy=r0|candidate` help line),
  `decisions.md` and rust-engineer memory. Clippy is clean and the tests pass: **1479 passed,
  0 failed**. `flow.rs`, the mirror and `fetch/caller.rs` merge without conflicts.
* **Branch + Firefox (a702d0a, 7938b23).** Same conflicts in `cli.rs` and `pipeline.rs`. The
  connector files (`backend.rs`, `caller.rs`, `fetch/mod.rs`, `app_actor.rs`) auto-merge. Clippy
  is clean, **1543 passed, 0 failed**.
* **Branch + M6 (855dd35).** Adds a conflict in `app_actor.rs` (`route_text` and `note_text_lost`
  are adjacent; keep both). Clippy is clean, 1597 passed and 1 failed: `gorca-rt
  sched::tests::this_process_lists_itself_as_other`, a B-5 code flake that passed 3 out of 3 on
  rerun.

## Round-1 findings: status (re-run probes, `probes.diff`, `probes.log`)

| Finding | Status | How verified |
|---|---|---|
| F-1 (1) timeout fallback | **closed** | r1 probe now fails: `["speak","settled","fetch-rv1"]` |
| F-1 (2) RV-1 error arm | **closed**, see RR-1 for its side effect | retry test; RR-2 for the residual |
| F-1 (3) same-app cached move demotes | **closed** | r1 probe fails: still published after the move and after an RV-1 timeout |
| F-1 (4) confirmed-after-trigger skip | closed | implementer's test; code `flow.rs:285-292` |
| F-2 GetChildren amplification | closed (fake app only) | listing capped at 512 KiB before decode; indexed path for parents with >256 children; no Prefetch while `Flooded` (`prefetch.rs:283-291`) |
| F-3 backward look-ahead | **partial**, RR-3 | new probes fail |
| F-4 ChildrenChanged in flight | closed | new probe `probe_rr_children_changed_during_first_window` passes (1 -> 2 windows). The r1 probe no longer exercises the bug: the first focus now asks for Ancestry first. |
| F-5 stamps in `memory_bytes` | closed | code |
| F-6 echoed-op bookkeeping | closed, see note | `siblings_op` clamps with the same formula as `FillDriver::sanitize`, so the result is a fixed point |

F-6 note: clamping is sound today because the policy's `before.min(cap)` /
`after.min(cap - before)` is exactly the driver's formula. Ancestry is not clamped by the policy
(24 against a limit of 64). A probe with `max_ancestry_depth = 2` still shows 0 misses, because
Ancestry is not tracked in a way that blocks. This coupling is fragile, not wrong. A unit test
asserting `sanitize(op) == op` for every op the policy emits would pin it down.

## New findings (ranked)

### RR-1 Medium: the RV-1 retry makes a slow, live app read as Hung twice as fast; one keypress can be enough
`crates/gorca-core/src/present/flow.rs:573-584` (retry on `Timeout`) and `:459`
(`present_cached(.., true, ..)` after a Focus timeout). Both RV-1 requests are `Interactive`
(`flow.rs:193`). `health.rs:17,218-221` counts consecutive timeouts of any class and enters Hung at 3
(`RestrictToCache`, "application not responding").
* **Measured with pipeline probe `probe_rr1_two_cached_focus_moves_on_a_slow_app_declare_it_hung`.**
  The app answers in 150 ms, against the 120 ms Interactive deadline. The test makes two focus
  moves onto cached nodes.
  * Branch: 4 timeouts, "application not responding" spoken, 3 out of 3 runs.
  * Main 6fc592b: 2 timeouts, app Slow, nothing announced, 3 out of 3 runs.
* **From the code:** a stale-NAME cached focus whose Focus fetch times out now costs Focus, RV-1
  and the RV-1 retry. That is 3 consecutive timeouts, so a single keypress declares the app Hung.
* **Failure scenario:** Firefox stalls for 150-200 ms while it lays out a heavy page. The user
  Tabs twice and hears "application not responding". After that, reads stay restricted to the
  cache until the probe succeeds (backoff 250 ms to 4 s).
* **Fix (small):**
  * retry RV-1 only while protection is unearned (`!f.protects(app)`); a protected app gains nothing from the retry;
  * do not retry on `Timeout` at all, only on `Busy` or `Protocol`, and rely on RR-2's earning path instead;
  * or send the retry as a class that does not feed health.

  Add RR-1 as a regression test.

### RR-2 Low: after the RV-1 retry fails, the focus is never earned until the next focus (the residual is real)
Protection is earned only at `flow.rs:374` (presentation) and `:497` (RV-1). `on_state_reply` never
earns it. Probe `probe_rr2_failed_retry_then_focused_state_reply_never_earns`: RV-1 and the retry
both time out, then a `StateChanged` re-read returns `FOCUSED`, and `published` stays `None`.
Effect: the text events of a cross-app cached focus (Alt+Tab into a busy app) are dropped. They
are now counted in `text_unprotected`, but not spoken until the user moves focus. Fix: in
`on_state_reply`, publish when the node is the presented focus, shows `FOCUSED` and not `DEFUNCT`,
and is not a selection item. That also makes RR-1's timeout retry unnecessary.

### RR-3 Low: the F-3 direction is tracked by run index, so every backward run extension flips it to "forward"
`crates/gorca-mirror/src/fill/prefetch.rs:597-602` stores `prev_pos = (parent, idx)`.
`grow_run(.., backwards)` prepends keys (`:781-788`) without shifting that index, so on the next
step `idx > prev` reads as forward travel.
* Probe `probe_rr_backward_walk_lookahead_direction` (40 backward steps, each with its look-ahead
  due): **8 of 40 look-aheads go the wrong way**, one after each extension.
* Probe `probe_rr_backward_travel_rereads_behind` (22 -> 21 -> 20): row 19 is not re-read.

This matches the branch's own M6 of 0.050 backward against 0.019 forward. Fix: store the previous
focus key and compare `run.keys.position(prev_key)` with `idx`. Alternatively, shift `prev_pos`
when a run is grown backwards or reinstalled.

### RR-4 Info: any failure of a listing marks the parent "big"
`prefetch.rs:839-850` puts any `Failed` non-indexed Siblings, including a plain Timeout, into the
indexed set (ring of 8). That costs only extra `GetChildAtIndex` calls (8-wide), and it is correct.

## Questions asked by the orchestrator

* **Hung-test relaxation (3 or 4): legitimate.** Ran 25 interleaved runs each at load 10-11.5:
  * main: 21 runs gave 3 and 4 runs gave 4;
  * branch: 22 runs gave 3 and 3 runs gave 4 (`hung-test-25x.log`).

  The race was already there. Main relaxed the same test itself (9 in 40 at load 12) and added a
  "no new request after 150 ms" assertion. Take main's version in the merge. The bad app has no
  cache, so RR-1's paths do not apply to this test.
* **INVALID sessions: justified, not cherry-picked.** `adopt-rev-quick` and `adopt-rev-final` carry
  the harness's own banner ("service-time fidelity check failed", p50 delta 0.65 and 0.60 ms,
  strict). If anything the invalidation hurt the branch: rev-final's 1.04x was its best ratio. The
  report leaves out that rev-quick was **1.42x** (6.09 vs 4.29 ms, also invalid).
* **L1c margin: small but real, not noise.** The reviewer reran an interleaved session, `rr-l1c`:
  walk 3 per arm, R0 = main 6fc592b, `taskset -c 2-9`, strict fidelity **pass**, slot load 8-16.6.

  | L1c p99 ms | median | min | max |
  |---|---|---|---|
  | R0 (main) | 3.43 | 3.36 | 11.4 |
  | branch | 3.98 | 3.52 | 4.90 |

  Median ratio **1.16x**, min ratio 1.05x. Across all four valid sessions the ratio is 1.07, 1.08,
  1.18 and 1.16. That is a consistent cost of about 5-15 %. It stays under the 1.2x gate with
  little margin, and at about 4 ms it is far below the 15 ms budget. Other values from the same
  session:
  * HIT.all 0.883 vs 0.550;
  * M6 0.027 vs 0.232;
  * STALE, LOST and L3.lost 0;
  * MSG.walk 114 vs 31 msgs/s.

  Accept it. Re-measure on a quiet host before calling 1.2x "met".
* **M6 F3 workaround (`term_key` publish): removable on the success path, after an e2e check.**
  With this branch, Alt+Tab back to a cached terminal still demotes the terminal at presentation
  (cross-app, fail closed). The cached focus is "unearned", so RV-1 is always asked, and an RV-1
  that shows `FOCUSED` publishes. Output therefore flows after one RV-1 round trip, not after the
  first key. Two gaps remain:
  * output inside that round trip is dropped, though counted;
  * after RV-1 and its retry both fail, nothing is spoken until the user moves focus (RR-2).

  Fix RR-2, then delete the publish in `term_key`, and add the M6 review's e2e step (focus the
  terminal, emit output before any key, expect it spoken). That e2e step was not run here.

## Required before merge
1. RR-1: limit or remove the Timeout retry; add the pipeline regression test.
2. Resolve the merge with main as above (main's hung test, the union of the CLI flags).

## Follow-ups
* RR-2 earn on a FOCUSED state reply, which unblocks removing the M6 workaround.
* RR-3 direction by key.
* The F-6 `sanitize(op) == op` test.
* L1c on a quiet host.
