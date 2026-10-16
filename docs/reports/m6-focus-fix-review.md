# M6 focus fix review (78fa165 / docs 5c99628)

Reviewer verdict: **MERGE**, with one medium follow-up (F1) that should land before M6 is called done. The V1/VT gate failures are **not** a regression. They come from host load and reproduce on main at the same magnitude.

Worktree: `<repo>/.claude/worktrees/m6-focus-review` (detached at 5c99628). Raw data is in `bench/results/e2e/rv-ab-{main,fix}-p{1..6}/` and `bench/results/e2e/m6-focus-review/`.

## 1. Code review: can the change lose or duplicate typed echo?

Per-key and output paths in VT/V1 cannot reach the new code:
- `reconfirm_focus` / `ask_state` (`crates/gorca-core/src/present/flow.rs:159-198`) runs only on a focus event repeated inside `DUPLICATE_FOCUS_WINDOW`. VT focuses once, in `focus_term` (`crates/gorca-bench/src/e2e/m6.rs:207`), before the probe mark. V1 has no keys at all.
- The removed `term_key` publish (`terminal.rs:280`) was a no-op whenever the app was already current, because `FocusedApp::publish_at` returns early when `cur == new` (`crates/gorca-rt/src/focused.rs:90`). In the fake, the first focus read shows FOCUSED (`fakeapp.py set_focus` adds the state before it emits), so the app is current for the whole session. The author's DIAG run said the same (0 warnings), and with the publish restored VT still failed.
- Races checked:
  - A stale state reply after a focus move is dropped by E-3 (`on_reply`, `r.epoch < epoch.current()`).
  - `on_state_reply` ignores replies for a node that is no longer presented.
  - `Reconfirm` is reset only on a real focus change.
  - No events are held while the read is pending; the read is fire-and-forget.
- Forged or background focus: the change grants nothing new. Protection is still earned only by a read that shows `FOCUSED`. The same app could already earn it by answering FOCUSED to the first read, or through the existing S1 `StateChanged` re-read. A focus that moves away first makes the reply stale (E-3).

## 2. VT + V1 A/B (fake VTE), interleaved, both arms pinned to `taskset -c 2-5`

Setup:
- Same e2e binary and harness for both arms. The only difference is the reader binary: `gorca-main` was built from 5c99628 with main's `flow.rs`, `reader.rs` and `terminal.rs`; `gorca-fix` is 5c99628 as is.
- The order alternates per pair. The seed is the same within a pair and different across pairs.
- Other agents' `gorca-hog` processes were running (up to 12 cores busy), and a debug build overlapped pairs 5 and 6.
- Attempt 1 failed in 15 s per run because I omitted `--impersonate-orca` ("no SetKeyGrabs"). It is a rig mistake, not product behaviour, and those result files are kept.

| pair (load) | main VT miss/extra | fix VT miss/extra | main V1 p99 | fix V1 p99 |
|---|---|---|---|---|
| 1 (52/40) | 18/13 (+1 double) | 14/12 | 41.7 | 39.6 |
| 2 (14/31) | 8/9 (+2 double) | 5/4 | 33.4 | 40.7 |
| 3 (28) | 0/0 | 6/9 | 38.0 | 37.4 |
| 4 (4) | **0/0** | **0/0** | **32.4** | **32.4** |
| 5 (30/47) | 0/0 | 8/2 (+1) | 38.3 | 69.8 |
| 6 (34/38) | 5/7 | 17/16 (+1) | 36.7 | 46.2 |

- At low load (pair 4) both arms are clean and identical.
- Main fails VT in 3 of 6 pairs, with up to 18 misses, more than the worst fix result in the author's runs.
- V1 p50 is 32.0–32.2 ms in every run of both arms; only the tail moves, and it moves with load.
- Fix-only failures: 2 pairs. Main-only failures: 0. That 2–0 sign split has no statistical weight, and with no code path linking the change to the measured behaviour I call it noise.
- The failing probes match the author's runs: Right at the end of the line wanted "blank", and echo arrived out of order around a key. That is harness timing under CPU starvation (10 ms echo against a key tap).

## 3. Findings

**F1 (medium) — a failed reconfirm read leaves the focused app unprotected for the whole focus.**
- Where: `on_state_reply` returns on `Err(_)` (`flow.rs:744`), and `Reconfirm::Asked` blocks any further ask (`flow.rs:175`).
- Since 78fa165 nothing else can recover the protection: the key publish in `term_key` is gone.
- Failure scenario: on a real VTE first focus under load, GetState times out or returns Busy. Every text event of the terminal is then dropped as `text_unprotected` (`app_actor.rs:325`), including typed echo, until the user changes focus.
- Proven by probe `bench/results/e2e/m6-focus-review/probes/probe_reconfirm_fail.rs`. It passes on 5c99628, which means the defect is present.
- Fix: on `Err(Timeout | Busy)` for a reconfirm read, retry once with a fresh deadline (the same pattern as `revalidate_retried`). Also, in `term_key`, when `focused_app` does not protect the tracked node's app, call `ask_state(t.node)` instead of the old blind publish. A key is a cheap, validated recovery cue that still earns protection only on FOCUSED.

**F2 (low, pre-existing) — a `harness_focus` test panic hangs the test binary instead of failing it.**
- `tabbing_through_a_form_is_heard` fails under load: the fake app is slow, the reader announces "application not responding", and the assertion at `harness_focus.rs:168` fires. The process then never exits: one tokio worker spins and the connector keeps restarting its drain against the dropped bus (`hang-debug-fix.log`).
- This stalled `cargo test --workspace` for over an hour.
- Feature-matched A/B, 20 interleaved pairs: main 5/20 hangs, fix 3/20. A first, unmatched run gave 3/20 against 12/20, but the fix binary was built with workspace feature unification and main with `-p`, so that comparison is void. Both runs are kept.
- Fix: run the test body on a runtime with `shutdown_timeout`, or catch the panic and cancel `shutdown` before rethrowing.

**F3 (info)** — `tracing::debug!` on every focus-lane message (`flow.rs:87`) is cheap at the default `info` level. Fine as is.

## 4. Gates
- `cargo fmt --check`: clean.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo test -p gorca-core -p gorca-rt -p gorca-text`: 571 + 98 passed, 0 failed.
- `cargo test --workspace --no-fail-fast` (load 35–45, nice 19, CPUs 8–15): 24 timing and e2e tests failed (desktop, e2e, ext-host hostile and fuzz, b1_ic, storm rate). These are load-sensitive suites and none is in the changed code. I did not A/B them, so they are unverified rather than cleared. `workspace-tests-loaded.txt` is kept.

## 5. Process note
I deleted one smoke result (`rv-smoke`, fix arm, V1 quick, p99 46.3 ms at load ~45, pinned to 2–5) by mistake. Its numbers are recorded here.
