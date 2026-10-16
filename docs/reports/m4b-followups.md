# M4b follow-ups (N-1 to N-6)

Source: `docs/reports/m4b-rereview.md`. Probes Q1-Q4 are now regression tests in
`crates/gorca-core/src/present/window/m4b_rereview_probes.rs` (P1-P5 kept beside them).

| id | change |
|---|---|
| N-1 | `Led` records when the focus utterance went out. A window answer within `REPLACE_WITHIN` (150 ms) sends the complete line (title, role, text, focus) at `Prio::Interrupt` in the same epoch, replacing the playing focus; later answers speak only the missing title/text as a notice. The led switches to `Queued`, so the focus is never spoken again. Q1, Q2, Q1b (late) assert it; the existing slow-read test changed to the Interrupt contract. |
| N-2 | `WindowCtx.last_input` (stamped for every key/control message) and `user_app`: an activation within 600 ms of input, or of a window the mirror reports `active`, makes its app the user's for 2 s; `alone_mode` treats it like the focused app (`Focused`: cancels stale speech once, still under the 3/s fair budget). Q3 (no signal: still a notice), Q3b (input), Q3c (active). |
| N-3 | At 8 pending activations the oldest entry of an app other than the focused/switched one is evicted. Q4 (user focused in the dialog's app first). If every entry belongs to those apps the global oldest goes. |
| N-4 | `AppRoute.hints` is an `Arc<AtomicBool>` shared with `NewApp`/`AppServer`; late verification calls `raise_hints` (flag + `PathTable::allow_hints`). Unit test `a_late_verified_shell_gets_its_hint_paths`. The end-to-end late-verify path itself needs a real bus and is not covered. |
| N-5 | A1 `a1_full_ms`: activation to the first utterance equal to the complete line, led or split. |
| N-6 | `ShellExtension::present` skips the name segment of an unnamed unit slider. |

## Gates (head 65f357b, clean tree)
- fmt, `clippy --workspace --all-targets -D warnings`, `cargo xtask deps`: clean.
- `cargo test --workspace --no-fail-fast`: 1876 passed, 0 failed, 17 ignored (load 15 to 21).
- `desktop.rs` 5x at load 21.5: 6/6 each time.
- e2e `bench/results/e2e/m4b-followups/20261016T042005Z.{json,txt}`: A1/N1/R1/R1s/K2/K3/K4, 3 repeats,
  `--impersonate-orca`, `GORCA_M1/M1_STRICT/M4/M4B/M4B_STRICT`, load 10.7 start, governor performance, git_dirty=false.
  All 29 gated rows PASS. A1 `a1_ms` p99 5.57 (5.26 to 5.70), `a1w_ms` 3.74, `a1f_ms` 3.23, `a1_full_ms` 5.42 (info),
  `a1_split` 0, `a1_wrong` 0, lost 0; N1 `n1_focus_ms` p99 0.43 (max repeat 2.54), 0 interrupted/wrong/lost;
  R1 0.45, R1s 0.48 (50k eps), K2 0.31, K3 1.17, K4 lost/doubled/leaked/flap 0.

## Caveats
- The idle host shows 0 splits, so the Interrupt path is exercised by unit tests only, not by the e2e.
- N-2 `active` trust: a hostile app can claim `active`; effect bounded by the per-owner Speak budget (Q5 bound).
- Worktrees need `.toolchain` (symlinks) for `bench/e2e/run.sh`, which sources `env.sh` itself.
