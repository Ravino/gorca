# Terminal flat review reads a stale screen

Date: 2026-10-17. Build under test: bfea13f. Fix: core review/terminal path only (ext-vte untouched).

## Report
Live GNOME Terminal with output arriving: flat review (numpad line/word/char) sometimes keeps reading an
old set of lines, also after Alt+Tab away and back; only a gorca restart cured it.

## Findings (code reading; the live failure itself was not reproduced, see "Evidence")
Terminal review reads the screen held by the extension (`term_review_model`, no IPC, ADR-0006 LS-1). Four
defects let that screen, or a model built from it, go stale without anything ever correcting it:

1. `pick_model` (review/commands.rs) served `review.cur`/`prev` whatever its source. A terminal model has no
   zones (`checks` empty), so `validity` says Valid for ever. Whenever the terminal was not tracked at that
   moment (tracking skipped or lost: `text_track` returns early outside focus/pass-through mode, a focus
   presentation that does not finish, a duplicate-focus drop) the old Terminal model was read for good.
   This matches "stuck on the same old lines". Fixed: Terminal models are served only by `term_review_model`.
2. The screen generation (`TermSnapshot.generation`) restarts at 0 for every tracking (each Alt+Tab makes a
   new extension) while the cached model keeps its `built_rev`, so equal counts across trackings made an old
   model look current. Fixed: the Reader puts a tracking serial in the high 32 bits.
3. Text events of an application that does not hold the protection are dropped in the app actor and only
   counted as `text_unprotected`; the terminal extension is never told (`text_dropped` is bumped only for
   queue overflow and floods). A terminal whose focus never earned protection (real VTE answers the first
   focus read without FOCUSED) therefore keeps the screen of its focus time. Fixed: a review step on a
   tracked terminal of an unprotected app reads the screen again and waits for it.
4. `text_dropped` was only examined when the next text message arrived. A loss followed by silence (burst
   then quiet) left the extension trusting a screen that lacks the last lines. Fixed: `text_losses()` is
   checked by the review step too.

Review step flow: `exec_flat` (attempts == 0) -> if the focused terminal is untracked, track it (silent in
review mode) and suspend (`What::Term`, 400 ms, shares the step budget); else if untrusted (lost events or
unprotected) ask the extension to re-read (`Replaced`) and suspend; `term_fetched` resumes the step with
`review_term_replied`. Timeout degrades to the held screen (the existing OB-4 path). Counter:
`TermReads.review_syncs`.

## Tests
Unit (review/tests.rs): lost events then silence -> screen read again before speaking; unprotected app ->
read again each step; untracked terminal -> never speaks the cached model, adopts and reads; new tracking
never matches an older generation. VS e2e (crates/gorca-bench/src/e2e/m6.rs): real gnome-terminal runs a loop
printing a numbered line every 100 ms plus 120-line bursts; every 12 s: Alt+Tab away and back, then numpad
review reads (current, previous, same line 4 s later, line above 3 s later). Gate `vs_stale` 0: the spoken
number may trail the loop's counter by at most 40 ticks; `vs_silent` 0.

## Evidence
- Baseline (bfea13f + VS): 120 s, 40 reads, 0 stale, worst lag 23 ticks (the rig does not reproduce the
  live stall; label `base`, bench/results/e2e/base/).
- Fixed: 240 s, 76 reads, 0 stale, 0 silent, worst lag 3 ticks (label `fix`).
- Gates: see the bottom section.

## Risks
- The live trigger is not proven; the fixes close every path found where the held screen is not refreshed.
  If it recurs, run with `RUST_LOG=gorca_core=debug` and check `reads.review_syncs` / `events_lost` in the
  shutdown counters.
- A terminal whose app never earns protection pays one screen read (~40-100 ms) per review key.

## Gates (final code, 3 repeats, `--impersonate-orca`)
- fmt, clippy --workspace --all-targets -D warnings, cargo test --workspace (all ok), cargo xtask deps ok.
- M6 fake (V1 VT VH V2 V3 VP VR VF VB, strict): all PASS; VR p99 0.305 ms, vr_ipc_calls 0 (tsr-m6).
- Real VV + VS (120 s each x3): vv_failures/screen/focus 0; vs_stale 0, vs_silent 0, worst lag 2 ticks (tsr-vv).
- R1/R1s/RS (GORCA_M4 strict): PASS, R1 p99 0.34 ms (tsr-k). K2 p99 0.245 ms, K3 p99 1.05 ms, lost 0 (tsr-k23, needs GORCA_M1=1; tsr-k shows K2/K3 failed only because M1 was not enabled).
- Results: bench/results/e2e/{base,fix,tsr-k,tsr-m6,tsr-vv,tsr-k23}/.
