# M6 implementation report: GNOME Terminal (VTE)

2026-10-15. Scope: `docs/plan/phase-2.md` section M6, ADR-0001 T-1 to T-4 and D-5, ADR-0007 (text and caret), ADR-0008
(product rules in extensions). Live checklist: `docs/plan/checklists/M6.md`.

## What was built

| Piece | Where | Notes |
|---|---|---|
| Extension interface | `crates/gorca-ext-api/src/terminal.rs`, WIT interface `terminal` in `wit/gorca-ext.wit` | one pure function per terminal: `step(now_ms, input) -> actions`, plus `snapshot()` and `configure()`. No clock, IPC or thread in the extension, so M5 can run the same crate as a WASM component (not yet in the `extension` world) |
| Policy | `extensions/ext-vte` (`Session`, `Screen`, `Prompt`) | T-2 line diff (head/tail trim plus LCS, bounded 160k cells), settle (T-1), prompt detection (markers plus `prompt_regex`), password prompts (`secret_regex`, built-in list), key-explained echo (typed, erase, motion, history, Tab), flood and repaint policy, Ctrl+C. 79 unit tests, a fuzz test (16,000 random keys against a real-VTE model), a replay test of a recorded real session |
| Host arm | `crates/gorca-core/src/terminal.rs` | owns clock, screen reads (`TextOp::Screen`), speech (epoch, prio, `sensitive`), key classification, timers; native build of `ext-vte` linked in (`gorca-core` -> `ext-vte` is allowed by rule 3/5) |
| Screen read | `TextQuery::Screen` (model, connector `text/mod.rs`) | `GetText(0, -1)` plus caret, never a negative offset (T-3); keeps the end of an oversized text |
| Review | `crates/gorca-core/src/review/terminal.rs` | `LineSource::Terminal` model built from the extension's snapshot (one zone per row), starts on the caret row, re-anchored by row, no IPC, no revalidation |
| Settings | `[terminal]` in `gorca-config` | `settle_ms`, `burst_lines`, `prompt_markers`, `prompt_regex`, `secret_regex` |
| Connector data | `toolkit.rs`, `events/session.rs` | VTE `Text` offsets are scalars (the table said UTF-8 and switched the text layer off for terminals); three text event classes registered |
| App actor | `app_actor.rs` | a flood that collapses text events tells the Reader (`dropped` counter), so the terminal reads its screen again |
| Fake VTE | `tests/harness/fakeapp.py --scenario terminal --term-trim` | escape sequences, VTE-style frames, `vte_assert` counter (a negative `GetText` offset is the `g_assert` that kills a real server) |
| e2e | `crates/gorca-bench/src/e2e/m6.rs`, `bin/e2e.rs` gates | V1 VT VH V2 V3 VP VR VF VB (fake), VV (real gnome-terminal) |
| Real VTE rig | `tests/term/` | real gnome-terminal-server in a private headless session; `vte_probe.py` records events; fixture and replay test |

## Findings that changed the design

1. **No text event ever reached a reader started alone.** atk-bridge and GTK4 emit `text-changed`, `text-caret-moved`
   and `text-selection-changed` only while a listener registered them; `REGISTERED_EVENTS` did not list them (they only
   flowed in a session where Orca registered them). The real-VTE run found it; M3's text layer had the same hole on any
   real application. Fixed in the connector (`events/session.rs`). Cost: every application now emits text events; the
   ingress coalescer (T-1) and `Flooded` (T-4) are the defence, there is no per-application opt-out (next point).
2. **P-11 answered by source: D-5 is not available.** `RegisterEvent`'s `app_bus_name` is stored by registryd but never
   reaches applications (`registry.c:1011-1029`, `bridge.c:793`). Per-application deregistration of a flooding
   background terminal cannot work with atk-bridge; T-4 is the defence. Measured: a background terminal flooding at
   60 frames/s costs the reader 2.2 % of a core and does not delay the focused terminal (VB, 0.45 ms p99).
3. **Real VTE text** (`docs/platform/vte-terminal-accessibility.md`, "Measured on a real VTE"): every row ends in `\n`,
   empty rows included, written trailing blanks are kept; delete payloads carry the deleted text; a typed character's
   echo follows its key by 2 to 5 ms, so key and screen change arrive in either order. The extension therefore holds up
   to four unexplained characters 60 ms for their key, drops typed characters that never showed up, remembers erased
   and popped characters, and answers motion keys from a queue (two arrow keys can be pressed before the first caret
   event arrives). The fuzz test found each of these races.
4. **Focus snapshot without FOCUSED.** The terminal's focus presentation was served from a snapshot older than the
   focus (`FOCUSABLE|SHOWING|VISIBLE|ENABLED`), so `FocusedApp` was never published and the app actor never routed the
   terminal's text events (`wants_text_events` false). **Not fixed at the root** (`present/flow.rs`, mirror, the other
   agent's paths): the terminal arm publishes the protection when the first key reaches the tracked terminal
   (`term_key`). The same hole likely affects every GTK application's text events; see Risks.
5. **Pacing instead of flow control.** Only say-all chunks report speech progress, so program output is paced by an
   estimate of how long it takes to say (55 ms per character, 150 ms to 2.5 s); any key resets it and drops what waits.
6. **The line diff has traps**: a trimmed trailing blank looks like inserted text; LCS may match a new prompt to an old
   identical one (the cursor line is forced new when lines were inserted above it); a flood of identical lines
   (`yes`) is "Same" and says nothing, correctly.

## Gates (private session, governor performance x16, load 4 to 15 from other work)

Command: `bench/e2e/run.sh <label> --repeats 3 --only V1,VT,VH,V2,V3,VP,VR,VF,VB --reader daemon:<abs>/target/release/gorca,--impersonate-orca --reader-env GORCA_M6=1`;
real terminal: `tests/term/run_real_vte.sh target/release/e2e run --repeats 3 --only VV ... --reader-env GORCA_M6=1 --reader-env GORCA_M6_REAL=1`.
Final results (head of the M6 branch, 3 repeats, median and range of the per-run p99): `bench/results/e2e/m6-gates-r3/`,
`m6-real-r3/`, `m6-strict-m1-r2/`, `m6-strict-m4-r2/`, real soak `m6-real-soak/`. Earlier labels (`m6-dev`,
`m6-real-debug`, `m6-final` to `m6-final5`) keep the failing runs of the development.

| Gate | Threshold | Result (median [min, max] over 3 runs) |
|---|---|---|
| V1 last frame to SPEAK | settle 30 + 5 ms | p99 32.7 [32.6, 32.8] ms, `lost` 0 |
| VT typed characters exactly once, ~9,670 random keys per run (typing, BackSpace, Left, Right, Enter) | 0 missing/doubled/extra | 0 / 0 / 0 in all 3 runs |
| VT key echo | p99 <= 3 ms | 0.51 [0.51, 0.53] ms |
| VH readline session | 0 failures | 0 |
| V2 flood (cat 1.75 MB/s, make, yes): reader CPU | < 60 % of a core | 1.59 [1.53, 1.61] % |
| V2 SIOCINQ p99, broker disconnect | < 1 MiB, none | 0 KiB, none; key echo in the flood p99 0.51 ms, 0 lost |
| V2 10 minutes (`m6-v2soak`, fake) | as above | CPU 1.4 %, SIOCINQ 0, key p99 0.50 ms, 0 lost |
| V3 Ctrl+C to speech cancel | p99 <= 2 ms, next prompt spoken | 0.23 [0.19, 0.31] ms, prompt 30/30, 0 stale |
| VP password | prompt spoken, echo once, 0 plaintext | 0 missing, 0 doubled, 0 occurrences in the presentation log and trace stderr |
| VR review of the held screen | p99 <= 3 ms, 0 app calls | 0.37 [0.36, 0.38] ms, 0 calls, 0 lost, 0 failures |
| VF full-screen (pager page, Down row, htop repaint silent, nano edit) | 0 failures | 0 |
| VB background flood (T-4) | key p99 <= 4 ms, 0 stray | 0.43 [0.39, 0.47] ms, 0 stray, 0 missing |
| `vte_assert` (negative offset to `GetText`) | 0 | 0 in every scenario |
| **VV real gnome-terminal-server (VTE 0.80)** typing `echo hello` | 0 missing/doubled/extra | 0 / 0 / 0 |
| VV readline steps (Left, Home, End, Enter output, Up/Down history, Tab completion, BackSpace, Ctrl+U) | 0 failures | 0 |
| VV key echo | p99 <= 3 ms | 0.51 [0.43, 0.56] ms |
| VV password (`read -s -p password:`, sudo's shape) | prompt spoken, echo once, 0 plaintext in trace stderr and presentation log | prompt 3/3, 0/0/0, 0 occurrences (logs non-empty) |
| VV screen (`seq 1000 1999 \| less`: first page, Space, `b`, Down, `q`; Ctrl+Shift+T new tab, Ctrl+PgUp back) | 0 failures | 0 |
| VV floods (`yes`, `seq 1 100000000`): reader CPU, SIOCINQ | < 60 %, < 1 MiB | 1.56 [1.52, 1.61] %, 0 KiB |
| VV key echo during the floods | p99 <= 4 ms, 0 lost | 0.61 [0.47, 0.62] ms, 0 lost |
| VV Ctrl+C stop | p99 <= 2 ms, prompt spoken | 0.13 [0.12, 0.28] ms, prompt spoken after both floods, 0 retries needed in these runs |
| VV server aborts (T-3 on the real server) | 0 | 0 aborts, 0 `CRITICAL` lines in the server's stderr |
| Strict M1 (K2, K2s, K3, K4) | M1 gates | K2 p99 0.26 ms, K2s 0.35 ms, K3 0.92 ms, K4 0 lost/doubled/leaked/forged |
| Strict M4a (R1, R1s, RS) | M4a gates | R1 p99 0.38 ms, R1s 0.41 ms, RS 0 failures |

Real soak (`m6-real-soak`, `GORCA_V2_SECS=300`, about 10 minutes of `yes` and `seq` in the real terminal): see the
2 x 300 s floods with keys typed into them: every VV gate passes, reader CPU 1.4 % (`yes`) and 0.4 % (`seq`) of a
core, SIOCINQ 0, key echo during the floods p99 1.76 ms (p99.9 3.8 ms), 0 keys lost, Ctrl+C stop 0.24 ms, server alive,
0 `CRITICAL`.

Other gates: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`,
`tools/check-deps.sh` clean.

## Known residual

* **VT history**: before the last fixes the same 10k run showed 1 to 4 unanswered keys per run (`m6-final`,
  `m6-final2` to `m6-final5` keep those failing results). Causes found one by one with the fuzz test and debug traces:
  two arrow keys answered by one folded caret event, a Left whose caret event the settle swallowed, an erase spoken
  as a "kill", a stale late-key hold, and finally the fake terminal wrapping the single-line session at 80 columns and
  not moving the cursor at the edge (`--term-cols 20000` now). The fuzz test (`extensions/ext-vte/tests/fuzz.rs`,
  16,000 keys, echo 2 to 7 ms and 10 to 13 ms) is clean; with the echo 24 ms behind its key (never measured on a
  real VTE) about half of the 40 sessions still show a misattributed answer.
* **8 h soak**: not run. The real-server evidence is the 3 VV runs plus a 10-minute real flood soak, each checking
  that gnome-terminal-server is still alive (`vv_server_aborts`) and wrote no `CRITICAL`. T-3 is enforced by
  construction (`Screen` reads use `(0, -1)`, the extension never sends an offset) and watched by the `vte_assert`
  counter of the fake VTE in every scenario (the fake's 10k-key VT run is the fuzz: random keys against the reader, 0
  negative offsets). A fuzz that sends hostile offsets straight to a real server would only test VTE, so it was not
  built.
* **Diff cost on the Reader**: ADR-0001 T-2 puts the diff in the app actor's job; it runs on the Reader. Measured cost is
  below the noise of the gates (a 60x240 screen reading every 200 ms in a flood); moving it is a follow-up.
* **Flooded focused terminal**: when the ingress collapses a focused terminal's events the terminal is re-read; not
  exercised by a gate (62 frames/s is far below the flood thresholds).

## Risks and open questions

* The first key after Alt+Tab publishes the terminal's focus protection; before it, text events of a terminal whose focus
  presentation lacked `FOCUSED` are dropped by the app actor. The root cause is in the focus-protection path
  (flow.rs, mirror cache) that another agent is changing; it probably affects every application's text events (M3 too).
* Registering text events globally raises the event rate of every application; measured only for terminals.
* zsh, fish, right prompts, autosuggestions, wide characters, `PS1` with colour: not special cased; the checklist asks.
* The real-VTE rig depends on the headless seat quirks (keyboard created by the first virtual key, lost first key).

## Review fixes (round 1, `docs/reports/m6-review.md`)

| Finding | Status |
|---|---|
| F1 large screen, wrong caret row | Fixed. The reviewer's probe assumed the connector keeps 2048 bytes; it actually keeps the last 64 KiB (`text::MAX_TEXT_BYTES`, not `decode::MAX_TEXT_BYTES` = 2048), so a 200x60 screen (12 KB; 24 KB in Cyrillic) was never clipped. The defect is real above 64 KiB: pre-fix, a 120x300 Cyrillic screen (72 KB) read row 30 for a caret on row 20 and Left read the wrong character (VF failed 2 steps, kept in `bench/results/e2e/m6fix-prefix-vf2`). `screen()` now keeps up to 256 KiB (the reply limit) and, if it must clip, cuts at a row boundary and moves the caret by the dropped scalars (`None` if it fell in the dropped head): `clip_screen` in `crates/gorca-connector-atspi/src/text/mod.rs`. Tests: connector unit tests (the probe's 40x100 case at 2 KiB, Cyrillic scalar-vs-byte case), three ext-vte session tests on 200x60 ASCII/Cyrillic screens (focus row, Left/Right, a password prompt on row 55), and VF steps 60x200 ASCII, 60x200 Cyrillic, 120x300 Cyrillic. |
| F2 `echo.typed_characters = false` ignored | Fixed in the host arm (`term_step`): the key is still claimed, but the typed character's own speech is dropped; the toggle takes effect at once. Test `echo_typed_characters_off_silences_the_typed_character_but_not_the_rest`. `typed_words` is still not supported in a terminal. |
| F3 focus workaround | Unchanged by instruction; TODO added in `terminal.rs` pointing at the review. |
| F6 English-only password prompt | Keywords are a data table (`SECRET_WORDS` in `extensions/ext-vte/src/prompt.rs`): English, Spanish/Portuguese, Russian/Ukrainian (strings checked against `msgunfmt /usr/share/locale/ru/LC_MESSAGES/{sudo,sudoers,Linux-PAM}.mo`; German and French prompts match through `pass`). Full-width colon accepted. No echo-off heuristic: VTE exposes no echo state. |
| F7 plaintext check too weak | VP and VV use secrets of rare characters and also count, in the part of each log written during the password phase, every secret character that appears as a value of its own (quoted, or after `key=`, `char:`, `text=`, ...) (`secret_leaks`, unit-tested). VP keeps its logs on failure. |
| F4 text events for every application | See the Firefox comparison below. |
| F8, F5, F9 | Not in scope (waits for M5 / backlog). |

### Firefox comparison (finding 4: text events registered for every application)

Real Firefox M7a set (B0 W1 BR1 BR2 W2 W2b W3 FM1 FM2, `tests/web/m7/run_m7.sh`), main `4066991` against this branch at
`855dd35` (code identical to `2898129`), both clean heads, 3 rounds of 3 repeats interleaved main/branch/main/branch/...
(`bench/results/e2e/m6fix-ff-{main,branch}-{1,2,3}`), host load 9 to 15 from other agents (per-run load in the logs).
Every gate passes in all 18 runs, 0 lost, 0 failures. Median of the per-run p99 over the three rounds (ms):

| Gate (ceiling) | main | branch |
|---|---|---|
| W1 (5) | 0.54 / 0.65 / 0.51 | 0.49 / 0.47 / 0.51 |
| BR1 (5) | 0.49 / 0.51 / 0.53 | 0.49 / 0.50 / 0.50 |
| W3 (15) | 0.25 / 0.25 / 0.24 | 0.27 / 0.25 / 0.24 |
| FM1 (15) | 0.32 / 0.34 / 0.38 | 0.93 / 0.32 / 0.33 |
| FM2 (5) | 0.52 / 0.50 / 0.51 | 0.55 / 0.53 / 0.51 |
| W2b (150) | 67.8 / 68.0 / 64.2 | 69.5 / 114.0 / 85.7 |

Only W2b (focus of a Firefox document with a hint, dominated by Firefox's own tree build) looked slower on the branch,
with wide ranges on both sides (main also reached 150.4 ms in one repeat). A dedicated A/B of 5 repeats each
(`m6fix-w2b-main-2`, `m6fix-w2b-branch-2`; the first pair, `m6fix-w2b-*-1`, died of ENOSPC when `/tmp` filled up and
produced no result): p99 median main 67.7 ms [52.2, 3005 (a cold start)], branch 70.6 ms [49.1, 133.0]; main failed
`untagged_warm` once, the branch 0 times. Verdict: noise; no regression attributable to the text-event registration.
The W2b warm-up is the same effect B-1 works on; keep it open until re-measured on the B-1 head.
