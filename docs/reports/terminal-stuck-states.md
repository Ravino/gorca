# Terminal stuck states (found by the state model)

Both bugs come from `docs/reports/reader-state-proptest.md`; their `#[ignore]`d regressions in
`crates/gorca-core/src/statemodel/regressions.rs` are now live tests.

## 1. term-degraded: tracked by the stand-in, never upgraded

Cause: `term_track` falls back to the core's `WatchOnly` session when the policy slot is busy or
disabled at focus time. `term_retrack_on_key` returned early for any tracked terminal, so the stand-in
(says nothing, claims no keys) stayed until the next focus event. A second defect hid behind it: an
attempt that found no policy at all (slot absent) still stamped the 500 ms `RETRACK_INTERVAL`, so the
next key, after the slot came back, was refused too (seed 8793 in the 20k strict run).

Fix (`crates/gorca-core/src/terminal.rs`, `ext.rs`):
- `TermSession::degraded()` (default false; `WatchOnly` true). The retrack check looks at tracking
  quality: a degraded tracking of the focused terminal is replaced as soon as a real session can be
  opened (`term_open_real`), at the next key. Nothing restarts while the policy is still missing, so the
  stand-in keeps its secret watch.
- `secret_hold` survives the upgrade (fail closed).
- Only a tracking that actually started uses up the retrack interval.

## 2. term-baseline: lost baseline swallows the first output

Cause (ext-vte `Session`): a failed or timed-out baseline read left the session `Cold` with no timer;
the first later screen event started a silent baseline, which absorbed that command's output.

Fix (`extensions/ext-vte/src/session.rs`): the baseline is retried by the session's own timer, 200 ms
doubling to 1.6 s, 5 tries, then it waits for an event. While no baseline has succeeded, a baseline
started by an event (or events during the read) is "orphan": the last `burst_lines` non-blank lines above
the prompt are spoken as output instead of swallowed. Unit tests: growing pause, bounded retries, orphan
output.

Also changed: `the_retracking_on_a_key_is_rate_limited` now states the new rule; `statemodel` added to
the xtask env-rule exemptions (a `#[cfg(test)]` module declared in lib.rs; the scanner only knows
in-file `#[cfg(test)]`); `term-degraded` and `term-baseline` removed from the model's KNOWN list.

## Does this explain the live "history Up silent until Alt+Tab"?

Plausibly in part, not proven. The symptom signature (Up/Enter silent until a focus event) matches
bug 1 when the policy slot was busy at the focus (WASM deployment, first focus while the extension host
was loading or a call timed out). It does not explain a native-policy daemon, where the slot never
degrades; `docs/reports/terminal-history-recall.md` is not on this branch, so it was not compared.
Bug 2 shows up as one swallowed first output, not a persistent silence.

## Evidence

- `GORCA_STATE_STRICT=1 GORCA_STATE_CASES=20000 cargo test -p gorca-core --release statemodel::random`:
  only `review-kept` remains (separate fix, commit 4652684 on another branch); no term-* ids.
- Gates: cargo fmt, clippy -D warnings, `cargo test --workspace`, `cargo xtask deps`: see the commit.
- E2E results: see the section below.
