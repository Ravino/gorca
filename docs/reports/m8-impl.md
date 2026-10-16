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
