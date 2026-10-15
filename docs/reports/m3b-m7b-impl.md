# M3b and M7b implementation report: Firefox fields, browse say-all, layout-aware quick navigation

Branch `worktree-agent-a702d0a17a733562c` (checkpoint `09ff3ef` plus the finishing commit). Placement follows ADR-0008: the
web engine and the text layer live in core and never branch on a browser; Gecko quirks are data in the AT-SPI connector
(`Toolkit::Gecko => TextUnit::Utf16Bom`, the `GetAttributeRun` attribute-name table); no W2 product rule was needed, so
`ext-firefox` stays an M8 item. Rig: `tests/web/m7/run_m7.sh <label>` (private session and a11y bus, headless gnome-shell on a
unique Wayland socket, private Firefox profile per launch). Results: `bench/results/e2e/<label>/`; failing runs are kept.

## What was built

| Area | Where | What |
|---|---|---|
| Gecko offsets (M3b) | `gorca-core/src/text/{mod,caret}.rs`, `gorca-text/src/seg.rs`, `directory.rs` | Firefox fields are followed in Gecko's own UTF-16 offsets: the window keeps the string with its phantom U+FEFF after each astral scalar, unit rules keep a phantom with its scalar, text-changed inserts get their phantoms back from the length, the verbalizer drops U+FEFF and U+FFFC. `MirrorDirectory::text_scalar` (the M3 skip) is replaced by `text_window_follows` (false only for UTF-8 byte offsets, VTE). |
| Misplaced edits | `text/caret.rs` (`MISPLACED_MAX`, `Absorb.alt`) | An insert reported 1-2 units after the known caret (typed, committed or scripted; not a paste) and a BackSpace/Delete reported away from where the key leaves the caret drop the window and accept the caret event where the caret really went, so the character is not spoken twice. Tests `a_commit_reported_one_past_the_caret_is_spoken_once`, `a_backspace_reported_at_the_caret_is_spoken_once`. |
| Contenteditable | `text/mod.rs` (`text_accepts`, `In::Retarget`) | Caret, edit and selection events of the paragraph inside a focused rich editable retarget the machine (CE-1); a vanished paragraph goes back to the root. |
| Formatting | `connector-atspi/src/text/format.rs`, `core/src/text/formatting.rs` | `info.formatting` (Insert+F): `GetAttributeRun` with defaults, mapped by the connector's per-toolkit table: font, size, emphasis, script, colours, alignment, language. |
| Field say-all, selection | `text/say_all.rs` | Say-all and Shift-selection reuse the M3 machines on Gecko offsets. |
| Browse say-all (M7b) | `core/src/browse/sayall.rs` | Insert+Down in browse mode reads the document's flat text with the native say-all engine; chunks are spoken as the line presenter speaks them; the virtual cursor follows the chunk the server starts; any key stops it. |
| Auto-read on load | `browse/*`, `config/settings.rs` | `browse.say_all_on_load` (default true): after a load the title is spoken and the page reads itself; tab switches and Alt+Tab do not start a reading. |
| Layout-aware keys | `crates/gorca-input-xkb` (new), `commands/compile.rs`, `daemon/config_task.rs` | Grabs use the union of the keysyms each physical key types in every configured input source (libxkbcommon via dlopen, 10 s re-poll): H/K/... and Shift+1..6 work on the Russian layout. |
| M7a deferrals | `browse/commands.rs`, `browse/present.rs` | Frame (M) and article quick navigation; Escape belongs to the page while the focused field's popup is open; Shift+digit headings. |

## Review of the previous agent's last change (scoring)

The checkpoint's last step replaced the window scoring of the field scenarios ("a speech before the next key") with an
"order-based" scorer. That scorer searched forward from the last match for the first speech with the expected text within
1.5 s, so a wrong or extra speech between two hits was skipped silently, and a missing speech could borrow the next
identical key's speech (the FC1h line repeats `😀`). It could hide real misses, so it was **replaced**, not kept:

- `align()` in `crates/gorca-bench/src/e2e/webtext.rs` aligns every speech from the first key on with the keys by a longest
  common subsequence (a speech may stand for a key only with the key's text, after the key, within 1.5 s, in order). An
  unmatched key is `lost`; a leftover speech is `fc_double` if it repeats the matched one before it, else `fc_extra`. All
  three are gated at 0 in every repeat (`fc_extra` is new). Six unit tests cover late-in-order, no borrowing, doubles, wrong
  text, too late, and unscored keys.
- FC1h derives what each key must say from the page's own caret events (offset in UTF-16 units from the event tap), not
  from a model: under load the private Firefox occasionally ignores a virtual-keyboard arrow, and a model then shifts every
  later expectation by one (run `m3b-cont-b`: 5 such keys, 35 "lost", 23 "extra", none of them gorca's). Keys the page
  did not act on are not scored and are counted in `page_dropped` (information; 0 in every final repeat). Keys gorca
  swallowed are gated (`fc_swallowed` = 0).
- `tests/web/m7/ax.py find` matched a name by substring in BFS order, so FE1's "Typing" found the shallower contenteditable
  "Typing region:"; every earlier FE1 result measured the contenteditable. It now prefers an exact name. Typing into the
  contenteditable is now its own scenario, FCE2.
- Window scoring stays for the M3 fake-app scenarios (`m3.rs`), where no browser lag exists.

What the gate still cannot see: a key that says nothing followed by the next key with the same text spoken twice would score
as two hits (the texts and order are identical); latency of the second would be high, and FC1h's p99 3 ms would catch it.

## Gates (before the review fixes; the gates at the final head are in "Review fixes")

Final run `bench/results/e2e/m3b-m7b-final/20261015T092518Z.txt` (release, 3 repeats, all 24 scenarios, host loadavg 9 to 12
from other agents' runs, governor performance). Rechecks with `gorca_text=debug` in `m3b-m7b-recheck*`.

| Gate | Median (max) | Limit | Result |
|---|---|---|---|
| FC1h caret event to speech p99 | 0.22 ms (0.24) | 3 ms | pass; lost/double/extra/swallowed 0 |
| FC1 Down p99 | 0.83 ms (1.0) | 15 ms | pass; lost 0 |
| FE1 input typing p99 | 0.48 ms (0.53) | 3 ms | pass; lost/double/extra 0 |
| FCE2 contenteditable typing p99 | 0.45 ms (0.46) | 3 ms | **1 double in 1 of 3 repeats** (1 of 1800 keys); rechecks 6 repeats clean |
| FE2 randomised session | 0 missing / doubled / key names / wrong | 0 | pass (5 to 13 doubles per repeat at the checkpoint) |
| FS2 Shift+arrows p99 | 1.27 ms (1.40) | 15 ms | pass |
| FSA1 / FSA3 field say-all | first chunk 3.7 ms (10.0); stop 0.39 ms (0.46) | 15 / 2 ms | pass; caret placement 0 wrong |
| FCE1, FT1, FP1 | 0 failures | 0 | pass |
| BSA1 browse say-all p99 | 0.36 ms (0.39) | 5 ms | pass |
| BAR1, BL1, FR1 | 0 failures | 0 | pass |
| M7a B0 W1 BR1 BR2 W3 FM1 FM2 | all pass | | pass |
| W2b gorca-side p99 | 120.7 ms (max repeat 3004 ms) | 150 ms | median passes; **one repeat had a 3 s trial without a warm-up (`untagged_warm` = 1)**; rechecks 6 repeats: 93.7 ms (121.5) and 57.2 ms (81.0), untagged 0 |
| Strict K2/K3/K4, R1 | K2 0.28 ms, K3 0.85 ms, R1 0.43 ms, K4 all 0 | | pass (`m3b-m7b-strict2*`; the first attempt without `--impersonate-orca` is kept in `m3b-m7b-strict*` as a harness misuse) |
| fmt, clippy `--all-targets -D warnings`, `cargo test --workspace` (1471), `cargo xtask deps` | | | pass |

## Q-E4: does Gecko offset conversion need a text-run cache?

**No, and no conversion is done.** Gecko's ATK text is the UTF-16 string with a phantom U+FEFF after each astral scalar, so
every offset (caret events, text-changed offsets and lengths, `GetStringAtOffset` ranges, selections, character count) is an
index into the very string the window holds. Measured (`crates/gorca-text/tests/qe4.rs`, `bench/results/qe4/`):

| Approach | Cost |
|---|---|
| Chosen: window in Gecko units | character unit 6 ns (no emoji) / 0.21 µs (emoji in the window); word unit 0.1 / 1.4 µs |
| Convert raw to scalar from the text in front of the caret | 0.4 µs at 1k scalars, 5.6 µs at 16k, 35 µs at 100k, 350 µs at 1M, plus an IPC read proportional to the caret position |
| TX-1 run cache (sorted phantom offsets) | lookup 5 to 9 ns, build 12 µs (16k) to 463 µs (1M), every edit shifts all later entries |

The only gap is the text of a `text-changed` insert, which lacks the phantoms its length counts: the Reader puts them back.

## Review fixes

Review: `docs/reports/m3b-m7b-review.md` (verdict: merge after fixes). Final gates were run at a clean committed head
(`f4630dd`, `git_dirty=false`) in `bench/results/e2e/m3b-m7b-final3-{ff,strict-k,strict-r1}` (3 repeats each; host load
average 5.5 to 6.6 at the start, other agents' runs alongside, governor performance). Earlier runs are kept, labelled:
`m3b-fix-dbl-1/2/3` (debug-logged FE2/FCE2, the doubles below), `m3b-m7b-fixes-ff` and `m3b-m7b-fixes-strict-k`
(aborted at `6edb328`: the harness `find` change broke W2/FM1/FM2, and the run was stopped), `m3b-m7b-final2-*` (head
`4bdf6de`: everything passed except the new `e2_resync` gate, max 30 in one repeat, see F-3d).

| Finding | Status | What changed |
|---|---|---|
| F-1 rich editable stolen | fixed | `text/mod.rs` `text_accepts`/`text_ancestry`: a node the mirror does not know may redirect tracking only on a caret event; text changes and selection of an unknown node are dropped. Test `unrelated_text_change_does_not_steal_the_rich_editable` (fails on the old code: tracked the clock, spoke "12:01"). |
| F-2 scorer blind spot | fixed | `webtext.rs`: latency of a match is measured from its own key's page caret event (or the key); new gated `fc_late` counts matches later than 50 ms. Test `a_silent_key_then_a_doubled_next_key_is_late_not_clean` asserts `late == 1` with the new measure and `late == 0` with the old one. |
| F-3 doubles | root causes found and fixed (three) | See F-3a to F-3d. |
| F-4 quirk in core | fixed | `AppInfo.text_insert_slack` (model), filled from `Toolkit::text_insert_slack()` (the quirk row `gecko-insert-offset`, Firefox 2, others 0) in the AT-SPI connector; the directory carries it per app and `Ctx.insert_slack` reaches the machine; core has no engine constant. Debug log names the quirk. Test `scripted_insert_after_caret_does_not_swallow_a_real_move_without_the_quirk` (GTK entry) passes. |
| F-5 W2b stall | not touched (B-1 owns the warm-up) | Rerun at the final head: gorca-side p99 72.2 ms (median over 3; max 80.2), `untagged_warm` 0, `lost` 0. No 3 s trial in 3 repeats here or in `m3b-m7b-final2-ff` (69.8 ms, max 101.8). The earlier 3 s trial is not explained. |
| F-6 FC1h | fixed | `page_dropped` gated at most 2 per repeat; speech after a dropped key must be nothing or a character the caret stood at or moved to around it, else `fc_extra` (test `speech_after_a_dropped_key_is_nothing_or_the_edge_character`). |
| F-7 U+FFFC | fixed | A unit that is only embedded-object marks is said "object", inside running text it is silent; only U+FEFF is always dropped. |
| F-8 smaller | fixed | Auto-read `loaded` is keyed by document root, not by app (test `the_load_complete_of_another_document_does_not_start_the_read`). Layout poll: reads only the `sources` key, rebuilds only when the layouts changed, one keymap build in flight at a time (a busy flag held by the blocking thread; the deadline sets a cancel flag the build checks between layouts), and a failed or late build keeps the previous table and logs a warning instead of silently falling back to US-only (startup still falls back, logged). Harness `find` is exact by default; `find_partial` is an explicit opt-in (W2, FM1, FM2 links). |
| decisions.md table | fixed | The blank line that broke the table is gone; rebase on main done. |

### Gates at the final head (`f4630dd`, 3 repeats, median (max over repeats))

| Gate | Result | Limit |
|---|---|---|
| FC1h p99 | 0.24 ms (0.25); lost, double, extra, late, swallowed 0; `page_dropped` max 2 | 3 ms; 0; at most 2 |
| FC1 p99 | 0.75 ms (0.78); lost, double, extra, late 0 | 15 ms |
| FE1 p99 | 0.45 ms (0.57); lost, double, extra, late 0 | 3 ms |
| FE2 | missing, double, key names, wrong, swallowed, `ft_failures` all 0; `e2_resync` 0 | 0; resync at most 3 |
| FS2 p99 | 1.07 ms (1.14); lost, extra, late 0 | 15 ms |
| FSA1 / FSA3 | first chunk 1.38 ms (4.13); stop 0.33 ms (0.43); caret wrong 0 | 15 / 2 ms |
| FCE1, FT1, FP1 | 0 failures; FCE1 lost, double, extra, late 0 | 0 |
| FCE2 p99 | 0.48 ms (0.57); lost, double, extra, late 0 | 3 ms |
| BSA1 p99 | 0.37 ms (0.39) | 5 ms |
| BAR1, BL1, FR1 | 0 failures | 0 |
| M7a B0 W1 BR1 BR2 W2 W3 FM1 FM2 | all pass (W1 0.49 ms, BR1 0.47 ms, W3 0.24 ms, FM1 0.35 ms, FM2 0.52 ms) | |
| W2b gorca-side p99 | 72.2 ms (80.2); `untagged_warm` 0; lost 0 | 150 ms |
| Strict K2 / K3 / K4 | 0.26 ms / 0.89 ms / all 0 (`not_consumed`, `app_lost`, `app_doubled`, `chord_leaked`, resent per 10k) | 5 / 15 ms; 0 |
| Strict R1 | 0.30 ms; `not_consumed` 0 | 3 ms |
| fmt, clippy `--all-targets -D warnings`, `cargo test --workspace` (1497 passed, 0 failed, 16 ignored), `cargo xtask deps` | pass at `f4630dd` | |

### F-3: the doubles were gorca output, in four shapes

Captured with `gorca_text=debug` in FE2 (`m3b-fix-dbl-1`: two doubles, `-dbl-2`: four) and FCE2 (`-dbl-3`: one).

- **F-3a stale typed-key expectation.** A key the page never applied (dropped or refused) stayed at the front of the
  expectation ring, so the next key's insert did not match it and was spoken as "unexplained text" 5 ms after the key's
  own echo (the second `z` and `h` in `-dbl-1`). `ExpectRing::consume_insert` now matches an insert that starts at a
  later entry and drops the stale ones before it. Test `a_typed_key_the_page_dropped_does_not_double_the_next_one` (spoke
  `["d","z","z"]` before).
- **F-3b far-off insert offset.** Gecko named a typed key's insert four units past the caret (`Insert{offset:9}` with
  the caret at 5), then moved the caret by exactly the insert. The slack of 2 did not cover it, the caret move was
  read as a new move, and the window held the text at the wrong place, so the second "space" was the character read
  from it (the FE2 `Space` double of the review, `-dbl-2`: four doubles in a row). When the connector's quirk row
  declares the engine, a typed key's own insert now sets the caret echo to `caret + len` whatever offset is named and
  drops the window. Test `a_typed_key_whose_insert_offset_is_far_off_is_still_spoken_once`.
- **F-3c stalled page.** A Firefox that stops answering for 1.3 s delivers 13 typed keys as one insert; the
  expectations had expired after 1 s, so the whole text was spoken again (`fc_extra`, `-dbl-3`). `EXPECT_TTL_MS` is now
  3000, and the stale-skip above keeps a longer life safe. Test `a_stalled_page_delivers_a_long_coalesced_insert_that_still_matches`.
- **F-3d harness.** A loaded Firefox sometimes does not apply a key at all (the page caret differs from the model).
  FE2 used to score the next actions against a diverged model (`e2_wrong` up to 9). It now waits up to 3 s for the page
  to catch up, then resets the model to the page and does not score that action (`e2_resync`, gated at most 3 per
  repeat; `e2_swallowed` gates gorca taking a plain typing key, 0 everywhere). Not a gorca fault: in `m3b-m7b-final2-ff`
  one repeat had 30 of them because the wait was 240 ms.

Result at the final head: FE2 and FCE2 0 doubles. In the debug runs after F-3b: FE2 10 repeats and FCE2 10 repeats
with 0 e2_double; the one FCE2 `fc_extra` there was F-3c.

## Open items

- Live checklist `docs/plan/checklists/M7b.md` not walked on the real desktop. Known gaps listed there (say-all in a
  contenteditable reads one paragraph; Ctrl+A in a contenteditable; live regions, dialogs, container announcements: M8).
- W2b 3 s trial seen once (at `09ff3ef`) is unexplained; the warm-up belongs to the B-1 branch.
- FS2 reported one `lost` in an aborted run at `6edb328` (evidence kept in `m3b-m7b-fixes-ff/lost-fs2-*`); 0 in the six
  full repeats at `4bdf6de` and `f4630dd`. Not understood; keep FS2 gated.
- Firefox's text-changed offsets are unreliable in more ways than the quirk row describes (F-3b); a measured model of
  them (astral characters earlier in the field) would let the row name the real rule instead of "any offset".

## Re-review fixes (round 2, `docs/reports/m3b-m7b-rereview.md`)

- **R-1 fixed.** `ExpectRing::consume_insert` takes `at_caret`: a match behind skipped (stale) keys is accepted only
  when the insert landed at the caret within the app's `text_insert_slack` and every skipped key is at most
  `SKIP_MAX_AGE_MS` (1 s) old. The reviewer's probe is now `a_scripted_insert_after_refused_keys_is_spoken` (GTK, core)
  plus `a_scripted_substring_of_refused_keys_is_not_their_echo` (ring). An unknown caret counts as not at the caret.
- **R-2 fixed.** `EXPECT_TTL_MS` is 1 s again for every toolkit. The 3 s life (`EXPECT_TTL_QUIRK_MS`) applies only to an
  app whose connector declares the `gecko-insert-offset` row (`insert_slack > 0`, so still data from the connector; the
  core has no engine name). Tests `a_stale_expectation_does_not_eat_a_scripted_insert_two_seconds_later` (GTK),
  `a_gecko_insert_two_seconds_late_is_still_the_typed_keys_echo` (Gecko), and ring-level
  `the_default_ttl_is_one_second_and_a_quirk_ttl_is_longer`. Using the slack as the "Gecko row" switch is a proxy; a
  second quirk value would become its own `AppInfo` field.
- **R-3 accepted, documented** (code comment in `caret.rs` `insert`): in Firefox a typed key's far-offset insert (a refused
  key plus a scripted append) counts as misplaced at any offset, so it can swallow a non-key mouse caret move to
  caret+1 within the 50 ms absorb window. Needs a measured Gecko offset rule to narrow.
- **R-4 fixed.** FS2 now derives what each key must say from the page's own caret events (as FC1h) and reports
  `page_dropped` (gate at most 2 per repeat, `FC1h` and `FS2`); a dropped key is unscored and logged in the run notes.
  Exemption counts (`page_dropped`, `e2_resync`) per run are in the results table below. Not done: FE2 still does not
  score doubles on a resynced action.
- **Info (ADR-0008 `disabled_quirks`)**: not implemented. It needs a setting in `gorca-config` (settings, load, schema)
  and a pass-through to the connector's quirk lookup; M6 also edits those config files, so it is a follow-up after M6
  merges (decisions.md).
