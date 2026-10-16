# M6 review (GNOME Terminal / VTE), round 1

2026-10-15. Reviewer. Branch `worktree-agent-ab80bab70d6090a95`, head `fca0d7e` (gate code last changed at
`47522d4`; the two later commits are docs only, checked with `git diff --stat 47522d4 fca0d7e -- crates extensions tests tools bench/e2e`).

**Verdict: MERGE AFTER FIXES.** Two must-fix defects (F1, F2), both small. Mark M6 as "automation done", not
"done": the 8 h soak, the real-server fuzz and the 30-item user checklist are still open (F5).

## What I ran

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| `cargo test --workspace` | exit 0, 104 test binaries, 1528 tests passed |
| `tools/check-deps.sh` | exit 0 |
| Fake-VTE gates, 3 repeats: `bench/e2e/run.sh m6-review-fake --repeats 3 --only V1,VT,VH,V2,V3,VP,VR,VF,VB --reader daemon:<wt>/target/release/gorca,--impersonate-orca --reader-env GORCA_M6=1` | **all gates PASS** (`bench/results/e2e/m6-review-fake/20261015T101755Z.txt`). Host load 6 to 18 (two B-1 soak runs of another agent in parallel), governor performance. `git_dirty=true` only because my probe files sat untracked under `extensions/ext-vte/tests/` (not linked into the daemon) |
| Hostile-input fuzz of `ext-vte` (`probe_hostile.rs`, 400 sessions x 3000 random inputs: offsets up to `u32::MAX`, 20k-char screens, combining/wide/NUL chars, invalid regexes) | 0 panics, worst single `step` 1.15 ms, largest snapshot 27.7 KB |
| Clipped-screen probe (`probe_clip.rs`) | **fails**, see F1 |
| Core echo-setting probe (`probe_core_echo.rs`) | **fails**, see F2 |

My numbers (median [min, max] of per-run p99): V1 32.7 [32.6, 32.9] ms (gate 35); VT echo 0.50 [0.47, 0.52] ms,
0/0/0 missing/doubled/extra; V2 CPU 1.54 % of a core, SIOCINQ 0 KiB, key p99 3.30 [0.55, **4.54**] ms (gate 4,
median rule); V3 0.29 [0.19, **2.91**] ms (gate 2, median rule); VP 0 plaintext; VR 0.40 ms, 0 IPC; VB 0.47 ms.
The implementer's numbers reproduce. The single over-ceiling runs for V2 keys and V3 coincide with load 14 to 18
and are within the median-of-repeats rule; worth watching, not a finding.

Probes are kept in `bench/results/e2e/m6-review/probes/` (copy into `extensions/ext-vte/tests/`, or append the core
one inside `crates/gorca-core/src/terminal/tests.rs` as `mod probe_echo { use super::*; ... }`).

## Gate honesty

* Thresholds match `docs/plan/phase-2.md` §2.1: V1 = settle 30 + 5 ms gorca (35, measured end to end including the
  settle, `v1_over_ms` is shown); V2 CPU < 60 % of a core, SIOCINQ < 1 MiB; V3 ≤ 2 ms. VT reuses E1's 3 ms; VR
  reuses R1's 3 ms; key echo under floods gets E1 + 1 ms (no plan number exists; reasonable). Every count metric
  (missing, doubled, extra, stale, failures, plaintext, server aborts, `vte_assert`) is gated at 0 on the worst
  repeat, not the median.
* Failing development runs are kept: `m6-dev` (35 FAIL rows), `m6-final` to `m6-final5`, `m6-real`,
  `m6-real-debug`. Nothing deleted.
* Not gated though measured: VT Left/Right answer latency (p50 10.6 ms, p99 11.1 ms, includes the fake app's caret
  delay) and `vt_output_ms` (42 ms). No gorca-side C1-style gate on terminal motion keys.
* Missing per PR-2: no V-scenario runs under the L3-50k storm (M3 had `C1hs`/`E1s`; M6 has no `V1s`/`VTs`).
* V2 is 60 frames/s of a 60x240 screen for 3 x 30 s (fake) and `yes`/`seq` for 2 x 300 s (real), not "cat of 1 GB
  for 10 min"; frame-rate-limited VTE makes this a fair proxy, but the plan's 10 min was only run as `m6-v2soak`
  (fake) and `m6-real-soak` (dirty tree, `git_dirty=true`).

## Findings (ranked)

### F1 High (must fix): a screen bigger than 2 KiB is read with a wrong caret

`crates/gorca-connector-atspi/src/text/mod.rs:76-91` (`screen`). `GetText(0, -1)` is clipped to its **last**
`MAX_TEXT_BYTES` = 2048 bytes (`decode.rs:18`) but the reply says `base: 0` and the caret is the offset into the
**full** text. Every consumer then indexes the tail with a full-text offset: the caret row lands rows too far down
(or clamps to the end), and the first row of the tail is a partial line.

*Failure scenario:* a maximised GNOME Terminal (≈50x190) running `nano`, `htop`, `less`, `man`, or simply showing
a screen of `ls -l` output, or any screen with non-ASCII text (Cyrillic is 2 bytes per char). Focus reads the
wrong line, Left/Right/Backspace/Delete read the wrong character (`session.rs:458-477` use `live.locate(caret)`),
review starts on the wrong row (`review/terminal.rs` `focus_line`), and password-prompt detection
(`secret_at(row, col)`) inspects the wrong row, so a password typed at a real `sudo` prompt is not marked
`sensitive`. Probe `probe_clip.rs`: a 40x100 screen (4000 bytes), caret on row 10; the unclipped run says
`row 10 …`, the clipped one says `row 29 …`. No gate covers it: every fake scenario that reads the caret uses
24x80 trimmed text, VV uses the default 80x24 window.

*Fix:* give `TextQuery::Screen` its own cap (ext-vte already bounds a screen at `MAX_SCREEN_BYTES` = 256 KiB,
`screen.rs:9`; `text_reply_fits` already bounds the D-Bus reply), and if it still has to clip, report the caret
relative to the kept text (`caret - dropped_scalars`, `None` when the caret fell outside) and start the text at a
row boundary. Add a VF step on a 50x200 full screen with the caret mid-screen.

### F2 Medium (must fix): `echo.typed_characters = false` is ignored in a terminal

`crates/gorca-core/src/terminal.rs:277-294` and `extensions/ext-vte/src/session.rs:405-431`. The terminal arm
claims every typed key and always says it; the setting and the `echo.typed_chars_toggle` command
(`commands/handlers.rs:147`) only reach the generic echo (`commands/keys.rs:52`) and the M3 text layer
(`text/mod.rs:299`). `typed_words` is not supported either. Probe `probe_core_echo.rs`: with the overlay off,
typing `x` in a focused terminal still speaks `["x"]`. A user who turns character echo off still hears every
character in the one application where they type most.

*Fix:* pass the echo settings in `TermConfig` (or have the host drop `Say{unit: Char, urgency: Key}` for typed
keys while keeping `ClaimKey` and the pending bookkeeping), and add the M3 test shape to `terminal/tests.rs`.

### F3 Medium (follow-up, tied to B-1 F-1): the focus workaround bypasses the protection rule

`crates/gorca-core/src/terminal.rs:281-290` publishes `FocusedApp` for the tracked terminal's application on any
key press. `present/flow.rs:359-371` deliberately earns protection only when the presented snapshot shows
`FOCUSED` and otherwise demotes (fail closed, B-1 adoption). The root cause the implementer found (focus served
from a snapshot older than the focus) is the open B-1 adoption finding F-1 (cached fallback never earns
protection; `docs/reports/b1-adopt-review.md`).

*User-visible effect of leaving it unfixed:* between a focus change and the first key, the app actor drops the
terminal's text events, so output that arrives then is never spoken (Alt+Tab back to a terminal where `make`
or `ssh` is finishing: the final lines and the new prompt are silent until a key is pressed). Typing is not
affected (echo comes from the key, and the first key publishes). That is real but narrow; it does not block
merge on its own. *Risk of the workaround:* an application that wins the Reader's focus without `FOCUSED` (the
forged-focus case B-1 guards against) gets protection from the user's next key.

*Fix:* land B-1 F-1 (earn protection on the revalidated cached path), then delete the publish in `term_key`. Until
then restrict it to `self.focus` == tracked node and not a switcher selection. Add a VV/VF step: focus the
terminal, emit output before any key, expect it spoken.

### F4 Medium (measure): text events are now registered for every application

`crates/gorca-connector-atspi/src/events/session.rs:34` adds `Object:TextChanged/TextCaretMoved/
TextSelectionChanged` globally (correct: without it M3 is deaf on real GTK apps). Firefox and Chromium then emit
`text-changed` for every text insertion during page loads. The fake apps emit regardless of registration, so no
existing gate measures this; only terminals were measured. Re-run the real-Firefox M7 gates (W2, W3,
`tests/web/m7/run_m7.sh`) on this branch before marking M7/M6 compatible.

### F5 Medium (process): acceptance not complete

Plan §M6 acceptance: "zero gnome-terminal-server aborts in the fuzz run and in an 8 h soak … a 30-item checklist
passes with the user". Done: 3 VV runs + 10 min real soak + the T-3-by-construction argument (no offset is ever
sent; `vte_assert` counted in every fake scenario). Not done: 8 h soak, a fuzz against the real server, the user
checklist, zsh/fish, wide characters. `docs/decisions.md` already says so; keep M6 open in the plan.

### F6 Low: password prompt detection is English-only

`extensions/ext-vte/src/prompt.rs:13`. A localised `sudo` (`[sudo] пароль для user:`), `passwd` or `ssh` prompt is
not recognised, so the typed characters are not marked `sensitive` (repeat/copy commands may hand them out) and
are kept in `pending` for up to 1 s. Logging is still clean. *Fix:* add localised keywords, or treat "typed
characters never echoed at a prompt that ends in `:`" as secret.

### F7 Low: VP/VV plaintext check only finds contiguous runs

`crates/gorca-bench/src/e2e/m6.rs:1316-1324` and `:2195` search for the whole secret and two halves. A per-key
leak (one log line per character, e.g. a key-event trace) passes. The fake VP also deletes its logs afterwards
(`remove_dir_all`, `:1327`), so a failure leaves no evidence. My grep of every `trace!`/`debug!` site in
`crates/` and `extensions/` found no text or key character logged; `TermAction`, `Utterance` and the terminal
input log are length-only. Third-party trace output (zbus at `RUST_LOG=trace`) was not audited. *Fix:* also count
lines that contain any single secret character in a key or text field, and keep the logs on failure.

### F8 Low (accepted for now): VTE policy coupling and isolation in core

Acceptable under ADR-0008 (`ExtBackend::Native` fallback) until M5, not a blocker:
* `gorca-core` imports the concrete `ext_vte::VteExtension` (`terminal.rs:17`), calls `ext_vte::unit_at`
  outside the trait (`terminal.rs:382`) and parses screens with `ext_vte::Screen` in review
  (`review/terminal.rs:13`). M5 needs these behind `TerminalHooks` (add `unit_at` to the hook or `snapshot`).
* `classify_term_key` (`terminal.rs:81-154`) hard-codes readline/emacs bindings (Ctrl+A/E/B/F/P/N, Alt+B/F) in
  core; this is shell policy and belongs in the extension (pass the raw key + modifiers).
* The extension runs on the Reader with no `catch_unwind` and no watchdog; a native panic takes down the Reader.
  The fuzz above found none, and the API demands it never panics, but ADR-0008's three-strikes host is missing.
* The diff runs on the Reader, not the app actor (ADR-0001 T-2). Worst measured step 1.15 ms on a 20k-char
  screen, at most ~5 readings/s under the 200 ms cap: acceptable; move with M5.

Dispatching on `Role::Terminal` in core is generic (WE-1), not an app special case. The VTE scalar-offset change
in `toolkit.rs` is connector data, as ADR-0008 asks.

### F9 Low: smaller items

* `text/mod.rs:81-82` (`screen`): text and caret are two sequential calls; the caret can belong to a newer frame.
  Harmless with the settle, but say so in the doc comment.
* Invalid `prompt_regex`/`secret_regex` are dropped silently (`prompt.rs:28-38`); `gorca-config` should validate
  them and emit a diagnostic like other keys.
* `review/terminal.rs` zone `src` ranges use the trimmed row length, so they drift from VTE offsets on rows with
  written trailing blanks. Unused today; fix before anything maps review positions to carets.

## Never-block, unwrap, FSMs

* All new IPC (`TextQuery::Screen`) goes through `spawn_text` with `Deadline::for_class` and the request path;
  admission control (`admits`) refuses reads from a flooded app; the extension has its own 400 ms fetch timeout
  and 2 retries. No blocking call found.
* No `unwrap`/`expect`/`panic!` in non-test code (clippy denies them; grep confirms). Wildcards in new code are
  only over keysym integers.
* `Phase`, `Expect`, `TermInput`, `TermAction`, `TermEdit` matches are exhaustive. All queues are bounded
  (`PENDING_MAX` 64, `LATE_MAX` 4, `MOTIONS_MAX` 8, `OUT_LINES_MAX` 400, `DP_CELLS_MAX` 160k, screen 256 KiB).

## Merge conditions

1. F1 fixed with a large-screen gate step. 2. F2 fixed with a test. 3. F3 tracked as a follow-up bound to B-1 F-1
(and the e2e step added when that lands). 4. F4 measured before M7 is called compatible. 5. Keep M6 open for F5.
