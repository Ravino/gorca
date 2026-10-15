# M3b + M7b re-review (round 2)

Reviewer, 2026-10-15. Branch `worktree-agent-a702d0a17a733562c`, head `4b01d66` (code at `f4630dd`). Fix round
`00bdce8..4b01d66`. Round 1: `docs/reports/m3b-m7b-review.md`.

Verdict: **MERGE AFTER FIXES**. One small fix is required (R-1). Every round-1 finding is fixed or explained.
The e2e gates and the code gates pass at head. The new stale-key skip in the expectation ring is generic: it applies to every
toolkit, not only to Gecko, and it can swallow an unrelated insert. Earlier code spoke that insert.

## What I ran

| What | How | Result |
|---|---|---|
| fmt / clippy `--workspace --all-targets -D warnings` / `cargo test --workspace` | own detached worktree `.claude/worktrees/m3b-rereview` at 4b01d66, clean tree (probes reverted first) | all pass; 1497 passed, 0 failed, 16 ignored |
| e2e FE2, FCE2, FC1h, 3 repeats | `tests/web/m7/run_m7.sh m3b-m7b-rereview --repeats 3 --only FE2,FCE2,FC1h --no-fail`, private session. Busy workstation: load 8.1 at start, 5 to 7 during the run, other agents' runs alongside, governor performance | **all PASS**: FE2 missing/double/keyname/wrong/swallowed 0 and `e2_resync` 0; FCE2 p99 0.43 ms with lost/double/extra/late 0; FC1h p99 0.21 ms with lost/double/extra/late/swallowed 0 and `page_dropped` 0. `bench/results/e2e/m3b-m7b-rereview/20261015T160549Z.txt` |
| Round-1 probes | re-inserted into `text/tests.rs`, then reverted | all pass (below) |
| New probes | `bench/results/e2e/m3b-m7b-rereview/probes/probe_text_r2.rs`, also run against the pre-fix code `864aae8` | 2 regressions, 1 accepted narrow case (below) |
| Trial merges | `git merge-tree` against main 6fc592b, B-1 (`agent-a259b4477ef251616`) and M6 (`agent-ab80bab70d6090a95`) | see R-7 |

The e2e header shows `git_dirty=true`. The cause: the probe edits to `text/tests.rs` (cfg(test) only) were made after the
release build, plus two untracked symlinks (`target`, `.toolchain`). The binaries under test were built from the clean 4b01d66 tree
("tree at 4b01d66" in the run log).

## Round-1 findings re-checked

| Round 1 | Probe / evidence | Now |
|---|---|---|
| F-1 rich editable stolen by a clock | `probe_unrelated_text_change_does_not_steal_the_rich_editable` | **pass**. Nothing is spoken, and the paragraph stays tracked. The fix drops the event; it does not suppress speech: `text_accepts` returns false before the machine sees it. |
| F-2 align blind spot | the implementer's `a_silent_key_then_a_doubled_next_key_is_late_not_clean` asserts `late == 1` with own-event latency and `late == 0` with the old measure, which is what my round-1 probe showed | **fixed**. `fc_late` is gated at 0 in FC1h, FC1, FE1, FCE1, FCE2 and FS2. |
| F-3 typing doubles | `probe_backspace_at_start_then_space_is_spoken_once` (GTK + Gecko, 3 gaps) passes; my e2e at head has 0 doubles in 3x FE2 and 3x FCE2; the implementer's debug captures `m3b-fix-dbl-1..3` | **root-caused**. The debug logs support F-3a, F-3b and F-3c, but F-3a and F-3c bring R-1 and R-2. |
| F-4 quirk in core | `probe_scripted_insert_after_caret_swallows_a_real_move` (GTK entry) and its baseline | **pass**. The slack travels as data: `Toolkit::text_insert_slack()` → `AppInfo.text_insert_slack` → `AppProfile` → `MirrorDirectory` → `Ctx.insert_slack`. Core has no connector import, and `cargo xtask deps` passed per the implementer (not rerun). Only Firefox is 2. Every other toolkit and the router placeholder are 0. |
| F-5 W2b 3 s | 6 repeats at 4bdf6de/f4630dd show max 101.8 ms and `untagged_warm` 0 | **not reproduced, still unexplained**. B-1 owns the warm-up. Keep it on the open list. |
| F-6 FC1h `page_dropped` | gate is `<= 2`. The final3 run hit exactly 2 in one repeat, my run had 0 | **fixed**. The bound has no margin (R-4). |
| F-7, F-8 | code read | **fixed**. The layout poll has a single in-flight build, keeps the previous table on failure, and reads only `sources`. `loaded` is keyed per document root and pruned. |

## Findings (ranked)

### R-1 MEDIUM (fix before merge): the stale-key skip matches an unrelated insert in any toolkit
`crates/gorca-text/src/echo.rs` `ExpectRing::consume_insert`. An insert whose text equals **any** contiguous run of unconsumed
expectations now counts as the typed keys' echo, and every older entry is dropped. Before this change, only a match at the front
counted. The change is not quirk-gated: it applies to GTK, Qt and Chromium too.

Probe `probe_r2_skip_match_eats_scripted_substring` (GTK entry): the user types `hello` into a field that refuses the keys
(maxlength or a read-only state; each key is spoken from the key). 500 ms later a script inserts `ll`. **Nothing is spoken.** At
`864aae8` the probe passes and `ll` is spoken.

Fix (about 10 lines): accept `skip > 0` only with positional evidence. Pass `at_caret` into `on_insert` and allow a skip match only when the
insert offset equals the caret, or is within the app's slack. Alternatively, allow a skip match only when the skipped entries are older than the matched one by at most
the old 1 s. The F-3a capture (stale `d`, then `z` inserted at the caret) still passes under both rules.

### R-2 LOW-MEDIUM: a TTL of 3 s for every toolkit widens the window in which a stale key swallows real text
`echo.rs` `EXPECT_TTL_MS = 3000`. The 1.3 s Firefox stall that motivates it is a Gecko observation, but the constant is global.
Probe `probe_r2_stale_expectation_eats_scripted_insert_2s_later` (GTK): `e` is typed and refused, and 2 s later a script inserts `e`.
**Not spoken** at head; at `864aae8` it is spoken. The probe also hits a caret shift that is spoken as "blank" in both versions.
That is pre-existing and not this round's issue. Fix: keep 1 s for skip matches and allow 3 s only for a front match, so the coalesced
stall insert still matches. Or make the TTL a connector quirk value like the slack.

### R-3 LOW (accept, document): with Gecko, an echoed insert at any offset can absorb a mouse move for 50 ms
`caret.rs` `insert()`: `echoed` makes `misplaced` true for any offset. Probe `probe_r2_far_echoed_insert_then_right_gecko`:
the user types `a`, the page inserts `a` far after the caret (a refused key plus a scripted append, so there is no caret event), and then a
**non-key** caret move to caret+1 within 50 ms. On Gecko it is silent. On GTK it says "w". With an arrow key it is spoken
on both. The window is 50 ms, it requires a typed-char match, and the move must not come from a key. Accept it, and record it in the quirk row
comment. The implementer's open item ("measure Gecko's real rule instead of 'any offset'") is the right follow-up.

### R-4 LOW: harness exemptions are bounded and justified, with two residual gaps
- FE2 `e2_resync` triggers only when the page caret differs from the model after a 3 s wait. Gorca output cannot cause it,
  and gorca taking a key is gated separately (`e2_swallowed` = 0). But a resynced action is not scored at all, including
  doubles. Bounded at 3 per repeat. Improvement: still count `e2_double` on resynced actions.
- FC1h `page_dropped <= 2`: the final3 run sat exactly at the bound (2 of 480 keys). Repeated speech of an allowed edge
  character after a dropped key is never counted as a double (`continue` before the `last_text` check).
- **FS2 has no page-dropped accounting.** The aborted-run FS2 `lost` (`m3b-m7b-fixes-ff/lost-fs2-*`) is explained: in
  cycle 8 the selection ends at `s` ("s selected", then "s unselected" with no `t`). That means Firefox applied only 19 of 20 Shift+Right.
  Gorca spoke every change the page made, and cycle 5 has the same shape. The loss came from the page, not from gorca. FS2 should derive
  expectations from page selection/caret events the way FC1h does, or it will flake on a loaded host.

### R-5 INFO: F-1 drops text changes on unmirrored nodes. That is correct for Gecko, but recheck it for Chromium
Probe `probe_r2_paste_into_new_paragraph_is_spoken` (an insert on a new, unmirrored paragraph) speaks at `864aae8` and is
silent at head. Gecko never sends that event: a new accessible's text is reported on its **container** as an embedded char
(`refs/firefox/accessible/base/NotificationController.cpp:311`, `new AccTextChangeEvent(container, offset, ...)`).
So with Gecko the probe is unreachable. The fallout is a pre-existing gap in both versions: a multi-paragraph paste into a contenteditable
arrives only as U+FFFC structure and is silent. Re-run this probe when Chromium rich editing lands, because Blink may fire on the new node.
`probe_r2_typed_into_new_paragraph_before_caret_event` passes ("x" once).

### R-6 INFO: ADR-0008 is only partly followed
The quirk is a match arm with an id in a debug log. ADR-0008:130 asks for a user-level `disabled_quirks` toggle and a
corpus test per row. Neither exists. This is acceptable for one row; track it for the second.

### R-7 Merge mechanics
- **main 6fc592b**: merges cleanly.
- **B-1**: only `docs/decisions.md` conflicts. `fetch/mod.rs`, `app_actor.rs` and `caller.rs` auto-merge.
- **M6**: 6 files conflict: `core/text/mod.rs`, `core/runtime.rs`, `config/settings.rs`, `config/load.rs`, `bench/bin/e2e.rs`,
  `bench/e2e/scenarios.rs`. Resolution rules: in `TextMsg::Caret`/`Edit` dispatch, keep M6's `if self.term_is(node)`
  **first**, then this branch's `self.text_active() && self.text_accepts(node, ...)`. `TextOp` keeps both `Screen` and
  `Format`. `TextState` keeps `term`, `format_req` and `was_active`. In `text_node_gone`, call `term_node_gone` and then this branch's root-based check.
  Semantics: M6 routes `Role::Terminal` to its own arm, so R-1 and R-2 do not reach VTE after M6. M6 registering text events for
  all apps sends more foreign-app events into `text_accepts`. They are filtered by `node.app != t.root.app`, and unknown same-app
  text changes are now dropped, so F-1's guard is what makes that safe. After resolving, run `cargo test -p gorca-core` and FE2 plus a
  terminal e2e. `gorca-text/tests/props.rs` (ring properties) is unchanged on both branches and passes.

### Unexplained items
- **W2b 3 s trial**: seen once at `09ff3ef`, not since (6 repeats). It is not attributable to this branch's code. B-1 owns it.
- **FS2 lost in the aborted run**: explained (R-4). The page dropped a key.
- **`run_m7.sh` output cut mid-summary**: only the console output was cut. The persisted `m3b-m7b-final3-ff/*.txt` and
  `final2-ff` hold all 24 scenarios with no FAIL rows in final3. The final2 `e2_resync` FAIL (30) is kept and labelled.

## Must fix before merge
1. R-1: positional or age guard on skip matching, plus the probe as a test.

Should fix: R-2 (TTL for front matches only, or a quirk value), R-4 FS2 page accounting. Follow-ups: R-3 measured
Gecko rule, R-6 `disabled_quirks`, R-5 Chromium recheck.
