# Terminal "silent after Alt+Tab": the missing piece was protection, not a timer

Date 2026-10-18. Base 9aabf7f6. Fix commit 367687d4.

## Finding
The kept trace (`bench/results/acceptance/rerun-vv-x20-20261018T180539Z/keep/`) shows a normal re-track
(untrack, track serial 8, FetchScreen, ArmTimer, Say, tick) and then no terminal event for 9 s. The
tick correctly armed nothing: ext-vte is event-driven when idle (`Phase::Idle` has no deadline), so
"no timer after the re-track" is by design. Every `ArmTimer` path in `term_step` was checked
(`due`, the runtime timer set, no discarded effect sinks): none loses a deadline.

What silences the terminal is upstream of it. `AppActor::route_text` forwards text events only while
`FocusedApp::protects(app)`. Protection is withdrawn when a cache-served focus is re-validated and
the real VTE answers before it reports `FOCUSED` (`on_revalidated` -> `demote()`), and after the
grace period the app's events are dropped (counted in `text_unprotected`, never surfaced). The one
reconfirm read per focus change was also left in `Reconfirm::Asked` forever when its reply lacked
`FOCUSED`. Only a key press re-read the state (`reconfirm_on_key`): "silent until the first key".

Confidence: medium. The trace has no per-event log of the demote; the mechanism matches every
observed fact (focus presented from cache, no `FOCUSED` in the states, silence until a key, rare and
timing dependent) and the 30 clean repeats below, but it is inferred from the code.

## Fix
- `Reader::term_protection_watch` (terminal.rs, called from `text_after` on every message): while a
  terminal is tracked and presented and its application has not earned the protection, the focus
  state is read again on a backoff (150 ms doubling to 2 s), one read in flight, never while a
  timeout is charged to the focus (B-1 RR-1), and only a `FOCUSED` reply earns protection (fail
  closed). Cost when protected: one comparison per message.
- `reconfirm_failed`: a reply without `FOCUSED` is `Failed(node, now)` instead of `Asked`, so the
  watch (and the key path) may ask again.
- Counter `TermReads::protection_reads`.

## Tests
- `an_unprotected_tracked_terminal_rereads_its_focus_state_until_it_earns_the_protection`,
  `a_protected_or_untracked_terminal_arms_no_protection_watch` (terminal/tests.rs).
- State model: `FocusedApp` is attached to the world, new invariant `term-unwatched` (a tracked,
  presented, unprotected terminal always has a pending watch). Strict 50k, seed 1: clean. The
  world's snapshots show `FOCUSED`, so the invariant rarely engages there; it guards the wiring.

## FE2 (test expectation)
`crates/gorca-bench/src/e2e/webtext.rs`: the FE2 model derives the boundary behaviour from the
config (`boundary_expect(CFG_QUIET)`): a key that cannot move the caret expects silence for
`cue`/`off`, `start`/`end` for `say`. Unit tests included. m7 FE2, 3 repeats, `e2_missing` 0 and all
other FE2 rows PASS on Firefox (`bench/results/acceptance/term-rearm-ff-fe2`) and Chromium
(`term-rearm-chr-fe2`).

## Real VTE VV x30
`bench/results/acceptance/term-rearm-vv-x30` (reader built from 367687d4, no quiet-host, load1
6-9): `vv_failures`, `vv_focus_failures`, `vv_screen_failures` all 0 over 30 repeats. Before: about
1 in 20-35. 0 of 30 is consistent with a fix but does not prove it (at 1/25 the chance of a
clean 30 is about 30 percent); the unit test is the stronger evidence.

## Open
- The demote path is not proven by a trace; if the flake returns, run with `RUST_LOG=gorca_core=debug`
  and look for `protection_reads` in the terminal counters.

## Review round (terminal-timer-rearm-verify.md)
- F-1 fixed: a stale StateChange reply for the reconfirm node turns Asked/Retried into Failed (flow.rs on_reply). Test `a_key_during_the_watch_read_does_not_wedge_the_watch`.
- F-2 fixed: a reply without FOCUSED for a terminal says nothing (title changes). Test `the_watch_reply_of_an_unfocused_terminal_does_not_speak_its_title`.
- F-3 fixed: a charged timeout arms no timer, other skipped attempts back off. Test `a_charged_timeout_does_not_make_the_watch_spin`.
- F-4: `Op::LateFocused` withholds FOCUSED for 400 ms after a focus; invariants `term-watch-stalled` (watch overdue > 300 ms) and `term-protection-lost` (6 attempts, healthy fetches, still unprotected). Mutation check: with the F-1 fix disabled the strict 50k run (seed 1) reports `term-protection-lost`; with it, clean for this id.
- F-5: not changed. The `None` expectation of FE2 already asserts silence (`wanted silence, said ...` counts as e2_wrong), so a speaking boundary key in cue mode fails.
- New, unrelated finding: the changed op mix exposes `cue-on-move` (strict 50k seeds 1 and 777777; Home in an area after lost events moves the caret and gives a cue). Ignored regression `home_after_lost_events_gives_no_boundary_cue_when_the_caret_moved`; not fixed here.
- VV x40 (`bench/results/acceptance/term-rearm-vv-x40`, reader 367687d4+fixes, load1 11-24, no quiet-host): 1 failing repeat of 40, and it is `VV a new tab: its output before any key` (heard only "blank"), not the Alt+Tab quiet step, which passed 40 of 40. The reader log is empty, so the cause is unknown; the earlier x30 (load 6-9) was 0 of 30.
