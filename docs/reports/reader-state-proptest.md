# Model-based tests of the Reader's mode and state transitions (2026-10-17)

Code: `crates/gorca-core/src/statemodel/` (`#[cfg(test)]`): `world.rs` (simulated desktop and effect
answerer), `mod.rs` (ops, generator, invariants, shrinker, the random test), `regressions.rs`.

## What it is
No proptest/rand in the offline registry, so: a seeded xorshift generator, `Vec<Step>` sequences, and a
delta-debugging shrinker (drop chunks while the same invariant id still fails, then flatten time gaps).
The Reader is driven only through `Reader::handle`; a `Sim` answers every effect (Fetch, TextFetch, ReviewBuild,
ReviewNav, ...) from a ground-truth world and fires every `ArmTimer` at its due time.

World: window, two text fields (entry, multiline area), button, terminal (prompt, Enter output, history
recall redraw, async output), a dialog that opens/closes. Ops: focus (duplicates, stale event ahead of the real
one), node removal/revival, dialog close with/without refocus, window activation, typed keys (letters, space,
punctuation, Backspace, Enter, Tab, arrows; with Shift/Ctrl/Alt), all review + object navigation commands,
echo toggles, notifications, extension speech, failing fetches, held (late, reordered) replies, lost text events,
app hung, extension slot Absent/Healthy/Busy, native vs WASM terminal deployment.

Invariants (ids): `echo-missing`, `review-kept`, `not-tracked`, `key-name`, `focus-lost`, `epoch`,
`term-degraded`, `term-silent` (+ `-after-review`, `term-baseline`), plus panic and timer-storm guards.
Run: `GORCA_STATE_CASES` (default 500, ~0.3 s), `GORCA_STATE_SEED`, `GORCA_STATE_LEN`; 50k cases = 19 s release.
`GORCA_STATE_STRICT=1` also checks the known bugs (they are otherwise switched off so they do not mask others).

## Bugs found
1. **review-kept (base: 49 % of random sequences).** Typing a letter/space/Backspace while review is on keeps
   review on; the text layer is inactive in review (`text_active()`), so echo falls to the generic path (space as
   "space", no deleted char, caret/terminal not followed). Shrunk: `Focus(entry); Cmd(ReviewCurWord); Type(Backspace)`.
   Fixed by commit 4652684 (not on main; verified: with that patch applied the regression passes and 50k
   sequences find nothing of this class). Regression: `typing_into_the_focused_entry_leaves_review` (ignored until merged).
2. **term-degraded (WASM deployment only).** The terminal policy slot is busy when the terminal gets focus: the core
   tracks it with the fail-closed `WatchOnly` session (reads nothing aloud, claims no keys, no snapshot). When the slot
   recovers nothing upgrades it: `term_retrack_on_key` returns early because a terminal *is* tracked. Typed
   Enter/Tab/Backspace/history are then silent until a focus event. Shrunk: `WasmDeployment; Ext(Busy); Focus(term);
   Ext(Healthy); Type(a)`. Suggested fix: `focus_audit_on_key` re-tracks when the tracked session is `WatchOnly` and the
   extension source now offers a terminal session (rate-limited like the existing re-track). This is candidate (2) of
   terminal-degraded-state.md made concrete. Regression `a_terminal_tracked_while_the_policy_was_busy_is_upgraded_by_a_key` (ignored).
3. **term-baseline.** Screen read behind a terminal focus fails (busy app), nothing retries while idle; the first
   successful read after Enter becomes the silent baseline: first command output swallowed. Shrunk: `FailFetches(true);
   Focus(term); FailFetches(false); Type(Enter)`. Suggested fix: a failed baseline schedules a retry (or the key audit
   re-reads when the extension has no snapshot). Regression `a_failed_baseline_read_is_retried_before_the_first_command_output` (ignored).

## Not bugs (harness lessons, kept as test notes)
- A text view echoing "tab" for a typed tab character is text echo, not a key name.
- Two false positives were harness artifacts: timers not fired at their due time (looked like silent history recall
  after review) and sub-millisecond rounding of due times (timer storm). Both are fixed in `Sim::settle`.
- Re-track (500 ms) and re-present (2 s) on a key are rate limited, so `not-tracked` is only asserted when the previous
  key is at least 2.1 s old.

## Not covered (needs more world)
Browse mode and Firefox ("stops browsing, speaks enter"): needs a doc actor/registry fake (browse/tests.rs has one);
no invariant for it yet. Speech-lane overflow/conservation counters live in the lane crates, not the Reader.
`Orphaned` focus (removed focused node, no refocus) is a known no-op and is excluded (world focus = none).
Contenteditable (paragraph retarget) is not simulated.

## Evidence
`cargo test -p gorca-core` 646 passed, 4 ignored; clippy `-D warnings` clean; 50k x 40 steps and 3 x 30k x 150 steps
(seeds 1e6, 2e6, 3e6) clean apart from the known ids. Base without the review fix: review-kept in 9772 of 20000.
