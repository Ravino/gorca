# Unified editing: adversarial pre-merge verification

Date 2026-10-17. Branch `worktree-agent-a12af19ad0bb6b976`, head `5b0d2ca8` (merge base with main `d003d0cb`).
Reviewer verdict: **MERGE AFTER FIXES** (must-fix: F1, F2, F3; F4/F5 before the report is quoted as evidence).

## Gates (run by the reviewer on `5b0d2ca8`, CARGO_BUILD_JOBS=4, RUST_TEST_THREADS=4)

| Gate | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace` (first run, fail-fast) | **1 failure**: `gorca-daemon/tests/term_parity.rs:184` `the_recorded_real_session_is_identical_in_all_three_runtimes`: `HostNative: ExtStatsSnapshot { faults: 1, resets: 1, busy_skips: 87, .. }` (host load from 3 other agents' builds) |
| `cargo test -p gorca-daemon --test term_parity` x3 | 5 passed each time |
| `cargo test --workspace --no-fail-fast` | 2140 passed, 0 failed, 17 ignored |
| `cargo xtask deps`, `cargo xtask strings` | ok (rule 5 is relaxed by this branch, see F3) |
| `cargo xtask build-ext` (wasm32 components) | ok, `ext_vte.wasm` 670 265 bytes (release) |

No `#[ignore]`, `#[allow]`, `unwrap()` or blocking call was added to non-test code (diff grep). The new
`passed_word` loop terminates (`w.start` strictly decreases). `Toolkit::edit_quirks` and the new `Op::Gtk4`
arm are exhaustive. No new IPC: never-block (ADR-0001 R3) is unaffected.

## Findings (ranked)

### F1 HIGH: the branch is stale against main; 22 files conflict
`git merge-tree --write-tree HEAD main` reports content conflicts in 22 files (every `AppInfo` literal:
main added `page_events_settle_ms` and `web_engine`; plus `toolkit.rs`, `directory.rs`, `builtin_ext.rs`,
`statemodel/mod.rs`). Main also changed Chromium contenteditable caret handling (`early_caret`,
`replay_early_caret`, `awaiting_hosts` in `gorca-core/src/text/mod.rs`, M9), which sits on the same path as this
branch's `enter_at`/new-block logic and the Chromium Backspace item (F5). Every gate and ED-* number in
`docs/reports/unified-editing.md` describes a tree that will not be merged.
Fix: merge main, rerun gates, ED-CHCE/ED-CHTA/ED-CHIN, ED-FF*, M3 and the terminal targets.

### F2 HIGH: terminal regression: the 100 ms edge timer speaks the wrong character when the caret event is late
`extensions/ext-vte/src/session.rs:63,410-423` (`EDGE_MS`, `flush_edge`). A motion key whose caret event has not
arrived within 100 ms is popped and the unit at the OLD caret is spoken; the real caret event that follows finds
no motion and is silent. On main the motion stays valid for `MOTION_MS` (250 ms) and the late caret event is
answered correctly. Reproduced with a probe (appended to `session/tests.rs`, then restored):

```
focus "$ abc def ghi" caret 2; CharRight; wait 120 ms; Caret(3)
P1 early=["a"] late=[]        (expected "b"; main speaks "b")
P3 (caret at 90 ms): early=[] late=["b"]   ok
```
Scenario: any ssh session or loaded host where key-to-echo exceeds 100 ms: every arrow key speaks the
character it left, then nothing. Fix (either): fire the edge only when the key provably cannot move
(caret at the prompt end for Left/Home/WordLeft, at end of input for Right/End), or keep the motion after the edge
speech and let a late caret event within `MOTION_MS` speak the correct unit (interrupting the edge speech).
Add P1 as a regression test.

### F3 HIGH (policy): `gorca-ext-api` now depends on `gorca-text` and transitively `gorca-model`
`crates/gorca-ext-api/Cargo.toml` (+ `pub use gorca_text as text` in `lib.rs:41`), `xtask/src/deps.rs`
(`EXT_API_AND_PURE_TEXT`). This contradicts the recorded licence decision (`docs/decisions.md`, 2026-10-17: core
GPL-3.0-or-later, SDK MIT OR Apache-2.0): a permissive SDK cannot re-export GPL crates, and it puts the whole core
object model (`gorca-model`: node keys, events, `AppInfo`) into the public extension API, which then has to stay
stable for third parties. The xtask unit test was also weakened: the "bad" case used to be
`ext-vte -> gorca-model` and is now `ext-vte -> gorca-core`, so an extension that depends on `gorca-model`
directly passes rule 5.

The dependency is not necessary. ext-vte uses only `present_word`, `Presented`, `CharsSpace`, `Lookup` and
`EditQuirks::default()` over a `Vec<char>` of one screen line (`session.rs:1283-1297`).

Options:
- **A (recommended, high confidence):** put a slice-based `word_passed(line: &[char], from, at) -> Option<Range>`
  (about 40 lines, `unicode-segmentation` word bounds) into `gorca-ext-api::terminal` (permissive) beside the
  existing `word_at`. Keep `gorca-text::present_word` as is. Enforce parity with one table of conformance vectors
  (`gorca_ext_api::terminal::WORD_VECTORS`) that both crates' tests run (`gorca-text` takes `gorca-ext-api` as a
  dev-dependency only). Restore rule 5 and its old "bad" test case.
- **B:** a new leaf crate (for example `gorca-text-units`, MIT OR Apache-2.0, no gorca deps) holding the word
  segmentation and passed-word rule; `gorca-text` and `gorca-ext-api` both depend on it. One source of truth,
  but a new crate whose permissive status must be policed (rule 5 allow-list = ext-api, i18n, that crate).
- C: compute the word host-side. Rejected: it moves terminal semantics out of the extension.

Separate, pre-existing on main (not this branch): `gorca-ext-api -> gorca-i18n` (M12a). `gorca-i18n` must then
also be permissive (it holds core catalogues and `build.rs` codegen); flag to the licence work.

### F4 MEDIUM: harness expectation weaker than the checklist, and the report overstates the matrix
- `crates/gorca-bench/src/e2e/edit_model.rs:152` `"BackSpace selection" => Exp::Some` (any speech), while
  `docs/plan/checklists/editors.md` row 25 says "the removed text". Recorded results show contenteditables never
  speak the removed text: ED-FFCE `[blank]` (7 of 8 runs), ED-CHCE `[one two | blank | blank]` (3) or silent (4).
  Native targets and textareas do speak it. So Backspace over a selection in a contenteditable is broken in both
  browsers, not only "intermittently silent in Chromium" as the report says.
- The report's matrix header says `ED_ASSERT=1`; no such variable exists in the code. ED-* scenarios only emit
  an `ed_failures` metric and notes; nothing turns failures into a FAIL verdict. Results are from dirty trees
  (`git_dirty=true`, e.g. `bench/results/e2e/ug-edc`).
Fix: make row 25 an exact expectation (removed text) with a documented per-target exception, correct the report.

### F5 MEDIUM (question 4): Chromium contenteditable Backspace-over-selection silence is pre-existing
Silent results exist before any presenter/quirk code: `bench/results/e2e/p5` (git_rev `8d784101`, i.e. main
plus the uncommitted harness) and `bench/results/e2e/r1-c` at `0f0c9c04` (harness + GTK4 fetch fix only).
The branch's `delete()` change only updates `t.caret` and cannot suppress speech. Confidence medium (dirty trees).
Main's later `early_caret` work changes this path: re-measure after F1.

### F6 LOW: `term_parity` flaked once under parallel load
One HostNative fault+reset (busy_skips 87) in the fail-fast run; 3 isolated reruns and the no-fail-fast run
passed. Native faults are a panic or a wall-clock `Overrun` (`gorca-ext-host/src/native.rs:53-54`); a recorded
replay would panic deterministically, so an overrun under load is the likely cause. Not shown to be new.

### F7 LOW: vacuous test assertion
`crates/gorca-core/src/text/tests.rs` `return_in_a_contenteditable_says_new_line_once_not_the_blank_block_too`
ends with `assert!(spoken(&fx).is_empty() || spoken(&fx).len() == 1)`: the "real move after Return" half checks
nothing useful. Assert the line that Up lands on.

### F8 LOW: docs
- ADR-0007 addendum says the indent is spoken as `tab`; after the M12a merge it is `1 tab`.
- The user requirement names the terminal model as the reference; this branch changed the terminal instead
  (Ctrl+Right now speaks the TL-1 word passed, `/usr/local` says `usr`, `local`, not the whitespace token). It is
  documented (ADR-0007, M6 checklist row 7) but the orchestrator should confirm the direction with the user.

## Not substantiated / not run
- No ED-* or M3/M6 e2e run by the reviewer (instructed: no full matrix). Parity claims for GTK3/GTK4/gedit/
  Text Editor/Firefox/Chromium rest on the committed results, which predate main's M9 changes (F1).
- `on_selection` now keeps any `Fetching` state and ignores the selection event (`caret.rs` ~1210). An
  uncorrelated selection (mouse) during a 150 ms unit fetch would go unspoken; not probed.
- Committed result files (527 under `bench/results`) follow existing repo practice; spot checks show only
  result JSON/TXT/topology and reader logs.

## Commands
`git merge-tree --write-tree HEAD main`; gates as in the table; probe module `reviewer_probes` appended to
`extensions/ext-vte/src/session/tests.rs` and run with `cargo test -p ext-vte reviewer_probes -- --nocapture`,
file restored from a copy afterwards; `grep` over `bench/results/e2e/*/*.json` for
`ED-* BackSpace selection`. Incremental dirs cleared after the runs.
