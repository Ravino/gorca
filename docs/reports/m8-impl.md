# M8 implementation report: Firefox browse mode at NVDA parity

2026-10-16, web-a11y-engineer (continuation of the first M8 agent). Branch `worktree-agent-adcbae30d4d7a4ad0`, based on
`b8521b1` (main at the M8 spec adoption). Spec `docs/plan/m8-spec.md`, baseline `docs/reports/m8-baseline.md`, gap table
`docs/reports/m8-gaps.md`, live walk `docs/plan/checklists/M8.md`.

## 1. Result

| | baseline `f80d1f5` | this branch (`m8-final2-full`, 3 repeats, head `d50ca23`) |
|---|---|---|
| scripted steps | 318 | 318 |
| PASS / GAP / XFAIL / unmarked FAIL | 181 / 55 / 82 / 0 | **308 / 2 / 8 / 0** |
| key p99 (gate 5 ms) | 0.33 to 0.43 ms | 0.33 to 0.48 ms |
| mode switch p99 (gate 15 ms) | 1.85 ms | 1.7 to 3.0 ms |
| live region, mutation to speech p99 (gate 100 ms = 65 + 35 rig) | not spoken | 81 to 96 ms |
| PF1 H / Shift+H at 100k nodes p99 (gate 5 ms), `m8-final2-perf`, 3 x 1200 keys | 0.56 ms | **0.52 ms**, lost 0 |
| first focus into an edit of a fresh page (`FM.edit.cold`) | 29 to 121 ms | 31 ms (load 12 to 18); budget 15 ms not met, see 5 |
| `cargo fmt`, `clippy --workspace --all-targets -D warnings`, `cargo xtask deps` | | pass |
| `cargo test --workspace --no-fail-fast` | | pass, except one flaky test (7) |

Non-PASS steps at the end: GAP `FM.invalid` (the error text of the field is not spoken), `FG.b.1` (the line shows the visible text "Menu",
not the aria-label); XFAIL `QN.o.2`, `QN.w.1`, `FM.tab.real`, `SY.tab`, `SP.back` (rig cannot type into the page), `RD.copy.say`, `EL.open`, `EL.find`.

## 2. What was built (this continuation; the first agent's part is in the log from `aaf878f`)

| Area | Where | What |
|---|---|---|
| Open failures of the first part | `doc_actor/live.rs` | LV.alert: the quiet window of a new page ran from the first batch, so an alert one second after load was "present text"; it now runs from the last bulk batch (>= 64 touched nodes, 600 ms). LV.log2 passes (digit folding). APP.toast: see 4. The `review::cluster` timing test passed in every full run |
| D-1 pass-through | `browse/forms.rs` | radio, tab, menu item, list item with selectable/focusable, grid cells on focus only; `role=application` and `<object>` are ATK_ROLE_EMBEDDED (raw role 78) and map to `Role::Application` in the document source (`roles.rs`), which gives Cat::EMBEDDED for O and focus mode on focus |
| D-2 Tab | `browse/commands.rs` (`web_timer`) | no key override (key injection is a stub and an ungrabbed Tab cannot be "not consumed" afterwards). The debounced scroll (150 ms) also sets Firefox's caret to the cursor (`SetCaret` on the cursor's text node, never in a form field or a widget, not while a selection exists); Firefox starts Tab from its caret when nothing is focused. **Unverified**: the rig cannot type Tab into the page (`SY.tab` stays expected-fail) |
| D-3 selection | `browse/commands.rs`, `commands/table.rs`, presets | 13 commands (Shift+arrows, Ctrl+Shift+arrows, Shift+Home/End/PageUp/PageDown, Ctrl+Shift+Home/End, Ctrl+A), virtual `(anchor, active)` in flat offsets, "W selected" / "W unselected", a plain move clears; mirrored into the page with `SetSelection` when both ends are in one text node, so the page's own Ctrl+C copies it. gorca+C (read the clipboard) not done |
| Keys | same | `,` and Shift+`,` (container end/start, "Not in a container", "bottom"), N (not a link block), A (annotation), W (invalid node, form fields excluded), P (paragraph), Ctrl+Up/Down (hard-line paragraphs), Alt+Up/Down (sentences, `cursor::paragraph`, `cursor::sentence`), gorca+Shift+Space (single-letter navigation off: the bare chords leave the grab set, `ModeKeys.letters`) |
| Announcements | `browse/present.rs`, `present/utterance.rs` | "out of list / table / block quote" when a move leaves one; "not checked" / "not pressed" for checkable roles in every focus speech (desktop too); `collapsed` from `expandable` without `expanded` (Gecko and GTK never set `collapsed`); a field's description after its value; application and embedded object as containers; list box name before its option; dialog name by node (the old offset test failed when the dialog appeared before the cursor's node) and also when the focused field joins the mirror late |
| Position restore | `browse/commands.rs` (`web_changed`) | after the cursor's node vanished, `Changed` re-resolves the cursor at the saved offset for 1.5 s while the user has not moved (re-renders arrive in several publishes) |
| Quick-nav fixes | `commands.rs`, `input-api/keys.rs` | previous landmark from a nested one; US shifted punctuation (`us_sym`: Shift+comma is `<`) so Shift+`,` is grabbed |
| First focus | `present/flow.rs`, `browse/commands.rs` | a cold focus on a node of a mirrored web document (not the document root, never a password's value) is spoken from the mirror and confirmed by the revalidation fetch (RV-1; a difference refines within the refine window). Cold first focus fell from 29 to 121 ms to about 31 ms |
| Check box activation | `browse/commands.rs` | the focus speech caused by our own click on a check box, radio or toggle is dropped, so Space says only the new state |
| Document re-focus | same | silent only within 2 s of our own click or Enter (anchors); a document that gets the focus again for any other reason is announced (B0) |

Unit tests added: `cursor::paragraph`, `cursor::sentence`, container jumps, single-letter grab set, selection, grab key sets
(`container_keys_and_the_letter_subset_are_grabbed`), D-1 roles. Core lib: 470 tests.

## 3. Harness changes (each justified)

| Change | Why |
|---|---|
| `TB.into`, `TB.span.hdr`: one Down instead of two | T lands on the caption line (the first line of the table), as in NVDA; the old two-Down step assumed the cursor stays above it |
| `SP.restore`: new page timer (re-render 1200 ms after the click) and steps `SP.rr.click`, `SP.items2`, `SP.rerender`, `SP.restore` | the old step did `DoAction` on the button, which moves focus and therefore the cursor to the button; Down then correctly went to the first row. Restore only makes sense with the cursor in the list when the page re-renders. The expected tokens did not change |
| `APP.toast` merged into `APP.close` (tokens "Compose" and "Dialog closed", window 3000 ms) | the page sets the toast in the same click handler that closes the dialog, so its speech belongs to that step's window; the separate step started after that window. Order is not asserted; I did not measure which of the two comes first |
| `FG.b.1`: `top()` before it | the menu button comes before the h1 the previous step stood on |
| `top()` before `QN.a.1`, `QN.w.1`, `QN.p.1` | those keys were expected-fail and never moved the cursor, so the steps never needed a start position |
| `esc()` before `APP.menu.item` and `APP.combo` | a mode word is spoken only on a change; the previous focus was already focus mode (APP.combo was not a gap, it was this precondition) |
| `IF.m.2` expects "Level two frame" | the page's frame title is "Level two frame"; "Nested level two" was a typo in the step. M is implemented |
| New expected-fail marks | `QN.o.2` (`<object>` with text/plain), `QN.w.1` (spelling is a text attribute), `RD.copy.say` (clipboard read) |
| Removed marks | all those whose features landed; none removed without the step passing |
| `e2e` `Firefox` removes its throw-away profile when the trial's Firefox has exited | /tmp (tmpfs) filled up: 130 MB per trial (coordinator request); logs stay |
| `heard` text truncated at a char boundary | a panic in the reporting of a multi-byte string |

## 4. Honest notes on the open items of the first part

- LV.alert: root cause above (quiet window), not the rig. APP.toast: the harness placed the check in the wrong window (3). LV.log2 passes.
- `harness_focus` (connector integration test) and two daemon e2e tests expected "check box" without "not checked"; updated. After a panic in
  `harness_focus` the test process hung in teardown (killed by PID; it is my own child process).
- The first full gate chain (`m8-final-*`, head `db58c43`) ran with a **bug of mine**: B0 failed 4/4/4 in all repeats ("entry: the first line was not
  spoken"): the silent document re-focus rule of the first part was too broad and a late-join lead consumed the document's first line. Both
  fixed (`b23efa4`..`d50ca23`); that chain is kept, labelled, and replaced by `m8-final2-*`.

## 5. Measurements (busy host, load 9 to 21; the quiet-host script was not used)

| Run | Head | Load (start/end) | Result |
|---|---|---|---|
| `m8-final2-full` (--quick, 3 repeats, all 20 scenarios) | `d50ca23` | 11.0 | 308 / 2 / 8 / 0 fail, no gate red |
| `m8-final2-perf` (PF1, FM, RD, 3 repeats, full samples) | `d50ca23` | 14.4 | PF1 p99 0.52 ms (0.51 to 0.53), lost 0; FM mode p99 2.2 ms (2.1 to 4.5) |
| `m8-final2-m7` (standard set incl. M3b typing gates, 3 repeats) | `d50ca23` | 12.1 | 90 PASS rows, 10 FAIL rows, all load-sensitive, see below |
| `m8-final2-strict` (K2 K3 K4 R1, `--impersonate-orca`, strict, 3 repeats) | `d50ca23` | 10.0 | pass: K2 p99 0.31 ms, K3 1.21 ms, K4 all 0, R1 pass |
| `m8-final-*` (earlier head, B0 bug) | `ecebf72`, `db58c43` | 12 to 20 | kept; `m8-final-full` PF1 lost 8 of 150 keys in one quick repeat (the 100k page's slow build phase; `m8-final-perf`, same head: 0 of 3600) |

`m8-final2-m7` red rows: L1 `lost` max 4, L1s `lost` max 64, W3 `lost` 1 trial, FC1h `fc_double`/`fc_extra` 1 and `page_dropped` max 6, FS2 `fc_extra` max 5,
`fc_late` 1, `page_dropped` max 13, FE2 `e2_missing` 1. All in single repeats of three, at load 11 to 13 with other agents working. The L1 loss
was compared with main (same host, back to back, load about 10): `m8-cmp-main-l1` and `m8-cmp-branch-l1` (3 repeats: main 0 lost, branch 1 lost in
one repeat), `m8-cmp-main-l1b` and `m8-cmp-branch-l1b` (8 repeats each: 0 lost on both). The typing and caret gates against main:
`m8-cmp-main-typing` and `m8-cmp-branch-typing` (see section 6). The separate K2-lost-under-load investigation was not touched.

First focus into an edit still costs about 31 ms against the 15 ms mode budget: the cold path waits for the document mirror to exist
(frontier fetch, warm-up) before the join hits; once the join exists the speech is immediate. The remaining cost is the mirror build for the
first focus of a page, not a fetch of the node.

## 6. Typing and caret gates against main (same host, back to back)

`m8-cmp-main-typing` (main `a456491`, load 12 to 13) and `m8-cmp-branch-typing` (this branch, load 12 to 13), FC1h, FS2, FE2, 3 repeats each,
back to back. Both are red in the same rows, as the M3b/M7b report documents for load above 10:

| Row (max over 3 repeats) | main | branch |
|---|---|---|
| FS2 `fc_extra` | 2 | 9 |
| FS2 `page_dropped` (keys the page dropped, not scored; gate 2) | 14 | 36 |
| FS2 `fc_late` / `lost` | 0 / 0 | 1 / 1 |
| FC1h `page_dropped` | within gate | 9 |
| FE2 `e2_resync` (gate 3) | 7 | 5 |

The branch is not clearly worse but the numbers are noisier on its side (FS2 `page_dropped` 36 against 14); three repeats at this load
cannot separate an effect from the load. The typing path (focus-mode text, `text/`) is not touched by this work except the `web_caret` hook
the first part added; browse mode is off in these fields. I did not chase it, as instructed for the K2 class. A quiet-host rerun of the two
labels is the way to settle it.

## 7. Known problems, risks, follow-ups

1. `b1-fakeapp` `closing_peers_ends_the_peer_connection_and_new_ones_work` failed once in `cargo test --workspace` under load and passes alone (3 runs); unrelated files.
2. D-2 is unverified; the caret is moved on every debounced cursor move in browse mode. A page with a `selectionchange` handler sees the caret moves. Needs the live walk.
3. A field's description arrives from the confirming fetch (`focus_need` adds DESCRIPTION for field roles); `FM.required` passes in the last full run, `FM.invalid` (error text) does not.
4. Button text in a line is the visible text, not the aria-label (`FG.b.1`): the mirror's flat text uses the children. NVDA reads the accessible name.
5. Elements list and find need a dialog UI (not designed). gorca+C needs clipboard access.
6. Spelling errors (W) need the `invalid:spelling` text attribute in the mirror.
7. `main` has moved on (M6, speech priority slot, M4b). `git merge-tree` against the main of this session: one conflict, `crates/gorca-core/src/text/mod.rs` (the `TextMsg::Caret` arm, where this branch adds `web_caret`); `lib.rs` merges cleanly. The command table (`COUNT`, `impl_names.len()` in `spec.rs`) must be rechecked after the merge.
8. Quiet-host gate-quality runs of the M7 set were not possible; the typing/caret gates are load-sensitive (documented in `docs/reports/m3b-m7b-impl.md`).

## 8. Review fixes (round 1, `docs/reports/m8-review.md`)

Merged `main` (M4b, follow-ups, M5, speech-lane priority, M6, ext-gnome-shell WASM) first, with the six
conflicts resolved as the review describes (`text/mod.rs` web join first, then the terminal branch;
`present/flow.rs` policy silence, web suppress, window hold; unions in `commands/mod.rs`, `lib.rs`; both sides'
tests and decision rows).

| Finding | Fix |
|---|---|
| H-1 | Caret follow removed (no `SetCaret` is sent from reading any more). D-2 as adopted: new commands `browse.tab_next` / `browse.tab_prev` (nvda presets, Tab and Shift+Tab). Target: `next`/`prev(Cat::FOCUSABLE)` from the cursor node on the mirror index, the document root excluded, the modal dialog respected; `Action::Focus`, so focus moves only on the user's Tab. Pass-through design: the Tab chords are in the browse grab set only for a direction that has a target (`current_grabs`, recomputed on mode entry and by the scroll-debounce timer, one `SetGrabSet` when it flips). With no target the key is not grabbed and Firefox gets it, with no injection needed. Costs: a Tab pressed inside the 150 ms after a cursor move uses the previous availability (stale grab: the target is still focused, or a spoken "no next focusable element" when the target is gone); a page change that adds a target after the cursor is noticed at the next cursor move; no wrap-around. Rig: SY.tab, SY.tab2, SY.stab and SY.tab.out (not consumed) pass. SY.nomove steps now also count page caret events, and K onto a link and Shift+Right in a link are checked (SY.nomove.k, .sel). |
| M-1 | `present_change` treats EXPANDED/COLLAPSED as one tri-state: "expanded" / "collapsed". |
| M-2 | `StateSet::MIXED` from AT-SPI `Indeterminate`; "half checked" in focus, change and browse speech, never "not checked" together with it. |
| M-3 | The live tracker's load window ends once for good (first quiet period); later bulk batches are re-renders and never hide a new alert. Test `an_alert_in_a_bulk_rerender_is_not_lost` (0, 63, 500 extra nodes). |
| M-4 | `Utterance.ttl_secs` (5 polite, 10 assertive, ADR-0003 13.2) honoured by the speech queue at dequeue and when full; an expiring item ranks below a non-expiring one of the same priority, so a Shell notification evicts live news instead of being dropped. Live speech goes through `notice_speak` (the owner's `Kind::Speak` bucket, newest-waiting). Tests in `gorca-speech/src/queue.rs` and `browse/tests.rs`. |
| M-5 | Virtual selection by default; `browse.native_selection_toggle` (gorca+Shift+F10) turns the page mirror on. In native mode a range is set only inside one text node that is not in a focusable element, otherwise (and when the range goes away) the page selection we set is collapsed. Ctrl+C grabbing and clipboard output are still deferred (gap table). |
| M-6 | The only Gecko rule in core was the caret follow; it is gone. Gecko role and state normalisation stays in `gorca-doc-source-atspi` / the connector decode. |
| 100k H/Shift+H key loss | Recorded, not chased: 8 of 150 in `m8-review-full` and one repeat of `m8-final-full`; main lost 7 in `m8-baseline-perf` (shared). |

Harness: FM.pass.tab and FM.pass.shift_tab ("Tab is not grabbed" in browse mode) are removed (`m8-fix2-full` keeps the Shift+Tab failure and the FM.tab.real.0 knock-on: Shift+Tab had already focused the first link), its premise is what D-2 replaces; `m8-fix-full` (head `5944ebc`) keeps the run where it failed (Tab moved focus to the next field, as designed). SY.tab, SY.tab2, SY.stab and SY.tab.out cover Tab now.

Gate results are in section 9.

## 9. Gates after the review fixes

All runs at a clean committed head (`git_dirty=false`) and `--impersonate-orca`; the host was shared with other agents (load 6 to 70), so
every row carries its load. Labels (all kept, including the red ones):

| Run | Head | Load at start | Result |
|---|---|---|---|
| `m8-fix-sy1` (SY, --quick) | `a74dd8c` | 18 | 16 of 16 pass, incl. SY.tab, SY.tab2, SY.stab, SY.tab.out (not consumed), SY.nomove.k/.sel (no focus or caret event) |
| `m8-fix-full` | `5944ebc` | 11 | kept: FM.pass.tab failed (premise replaced by D-2) |
| `m8-fix2-full` | `32ce45c` | 15 | kept: FM.pass.shift_tab failed, same cause; `m8-fix2-perf`/`-m7`/`-strict`/`-shell` ran at load 39 to 70 and are not evidence (non-M8 rows such as K2 and FC1 fail too); the shell gates were skipped there (env not passed), rerun below |
| `m8-fixab-{main,branch}-*` | main `ae79840`-era vs `fa5b19d` | 10 to 52 | interleaved main / branch / branch / main, strict K2-K4, typing FC1h FS2 FE2, A1 N1 R1s, 1 repeat per arm: every red row is red on main at the same load (main r1 load 52: K2 lost 61, A1 p99 123 ms; branch r1 load 28: K2 lost 3); at load 10 main A1 13.6 ms, branch A1 15 to 22 ms at load 27 to 31 (not same-load, see fix4/fix5) |
| `m8-fix4-full` (--quick x3) | `fa5b19d` | 44 | median 0 unmarked failures; FM.listbox, AF.field, AN.para.go failed in one repeat each (load); LV live p99 103 ms (gate 100); PF1 100k p99 8.1 ms, lost max 5 |
| `m8-fix4-strict`, `-shell`, `-m7` | `fa5b19d` | 35 to 37 | load-bound reds (K2/K3 lost, A1 p99 36 ms); M7 set 67 pass rows, reds in single repeats (FC1h, FS2, FE2, L1) as on main |
| **`m8-fix5-full`** (--quick x3, all 20 scenarios) | `3fffcb3` (merged with main `08a11b3`) | 25 | no step failure; PF1 100k p99 14.7 ms (min 7.4, max 66) and lost max 1: the shared 100k key-loss, recorded, not chased; PF2 one failed run of three |
| **`m8-fix5-strict`** (K2 K3 K4 R1 strict x3) | `3fffcb3` | 15 | K2 p99 0.32 ms, K3 1.25 ms, R1 pass; K4 chord_resent max 7 in one repeat (median 0) |
| **`m8-fix5-shell`** (A1 N1 R1s strict x3) | `3fffcb3` | 18 | A1 p99 6.7 ms, N1 0.45 ms (never interrupted), R1s 0.49 ms; A1 lost max 1 in one repeat |

The 100k-node H/Shift+H key loss (8 of 150 in `m8-review-full`, 5 and 1 here) is shared with main and is left to the perf owner.
`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test --workspace` (0 failed) and `cargo xtask deps` pass at `3fffcb3`.

## 10. Re-review fixes (round 2, `docs/reports/m8-rereview.md`)

First `m8-rereview.md`, the probes patch and the reviewer's results were committed, then `main` (03e6f9b and later) merged (only `decisions.md` conflicted, both rows kept) and the merged tree compiled. The probes are now regression tests and rig steps.

| Finding | Fix |
|---|---|
| R-1 | `gorca-input-mutter/src/source.rs`: ordinary keys that are down are tracked by keycode (`Down`, refreshed by every autorepeat). A grab set that adds or removes a chord/exact grab for a held key (Tab and ISO_Left_Tab count as one key) is deferred until its release, or until the key has been silent for 1.5 s (a timer arm in the loop, so a lost release cannot block grabs for ever). Test `a_keystroke_grab_is_never_added_for_a_held_key`; rig steps `SY.hold.*` (press, autorepeat, release through the Mutter emulation: the repeat and the release are not consumed). Together with R-5 the scenario no longer grabs Tab at all after a focus-driven move. |
| R-2 | `BrowseState.tab_grab` is a `Cell` written by `current_grabs`, the only place a pushed set is built, so it cannot go stale. Test `r2_the_tab_grabs_follow_the_live_targets_after_a_foreign_grab_push` (Escape then Ctrl+Home). |
| R-3 | `fair::Entry` has a second waiting slot for urgent utterances (priority above Notification). Polite news coalesces latest-wins in its own slot, an alert is never replaced by it and is released first. Tests `an_urgent_utterance_waits_in_its_own_slot_and_is_spoken_first`, `r3_an_alert_waiting_for_the_budget_is_not_replaced_by_polite_news`. |
| R-4 | `tab_target` walks from the cursor node and skips the document root, embedded documents (`Cat::FRAME`) and, backwards, the focusable ancestors of the cursor; `focusable_around` (native-selection guard) returns the nearest non-frame focusable. Test `r4_shift_tab_inside_a_focusable_container_finds_the_link_before_the_cursor`. |
| R-5 | NVDA `_tabOverride` / #5227: while the cursor was last put there by a focus change onto a control (`CursorFrom::Focus`), or sits on or inside the focused control, Tab is not grabbed and the browser's own order applies. Any cursor command of ours ends it. The grab set is pushed at once on such a focus move (no scroll timer is armed by a focus). Frame documents are not Tab stops. Test `r5_...`; rig steps `IF.tab.*`, `SY.tab2`, `SY.btn`, `SY.up3`, `SY.stab`. |
| R-6 | A failed `GrabFocus` (false, error, timeout) sends `DocNotice::FocusFailed` and says "can't move focus there". Native selection toggle outside a web page says "native selection is only available on web pages". `mixed_word(role)`: check box and radio "half checked", toggle button "half pressed", progress bar "indeterminate", nothing for other roles; a mixed toggle no longer says "not pressed". Tests in `utterance.rs`. Rig steps `FM.letters.tab`, `FM.letters.stab` (Tab and Shift+Tab pass through in focus mode). |

Rig notes: in this rig a key typed into Firefox through the compositor lands on a browser panel, so after the hand-back (R-5) the steps only check "not consumed" (`SY.tab2`, `IF.tab.2/3`, `IF.stab`); the real Tab result is covered by `SY.hold.*` and the manual checklist. `LV.assertive` failed once in `m8-rr5-full` (Firefox delivered the assertive event together with the next alert, 2 s late; the window is 1.5 s): load flake, green in `m8-rr6-sy-lv`.

### Gates (all at a clean committed head, `--impersonate-orca`; load and `/proc/pressure/memory` at start)

`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test --workspace` (0 failed) and `cargo xtask deps` pass. The `.toolchain` symlink was committed by mistake in one commit and untracked right after (`52ffc67`).

| Run | Head | Load | Memory pressure some avg10 | Result |
|---|---|---|---|---|
| `m8-rr2-full`, `m8-rr3-full` | `7627f99`, `be1a181` | 25, 15 | 0.00 | kept, failed on the way: IF.tab.2/3 (Tab grab not pushed on focus move, fixed), then SY.tab2/SY.stab (premise replaced by R-5, steps rewritten). `m8-rr2-m7` and `m8-rr4-m7` are aborted partial runs (source edited during the chain) |
| `m8-rr4-full` (SY IF FM, 3 repeats) | `feab4b0`-era | 12 | 0.00 | SY.stab silent (target already focused), step fixed |
| `m8-rr5-full` (full corpus, 3 repeats) | `feab4b0` | 7 | 0.00 | PF1 100k p99 0.48 ms, lost 0; two step failures: LV.assertive (1 of 3, flake above), SY.hold.ffocus (focused a node that was already focused; step fixed) |
| `m8-rr6-sy-lv` (SY LV IF FM, 3 repeats) | `7697d7f` | 8.5 | 1.10 | 0 failures, 0 XPASS |
| `m8-rr5-shell` (A1 N1 R1s strict, 3) | `feab4b0` | 8 | 0 | A1 p99 5.2 ms (max 7.6), N1 0.41 ms, R1s 0.43 ms, lost 0 |
| `m8-rr5-strict` (K2 K3 K4 R1 strict, 3) | `feab4b0` | 11 | 0 | K2 p99 0.28 ms, K3 1.12 ms, K4 leaked 0 and resent 0, R1 pass |
| `m8-rr5-m7` (standard set, 3) | `feab4b0` | 10 | 0 | reds in single repeats: L1s lost 1, FC1 fc_double 1, lost 1, fc_late 132, FE2 e2_double 1 |
| `m8-rr6-fc` (FC1 FC1h L1s FE2 FS2, 3) | `7697d7f` | 29 (other agents) | 1.1 | FC1 fc_late 54, lost 1; FC1h page_dropped 10: single repeats at load 29, the typing gates that are red on main at load >= 10 |

The M7 typing reds are not cleared at low load (as in round 1). The full-corpus run `m8-rr5-full` predates the two step-only fixes; the affected scenarios were rerun in `m8-rr6-sy-lv` (non-step code is identical at `feab4b0` and `7697d7f`).
