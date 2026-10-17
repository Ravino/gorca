# M8b review: ARIA names, roles, states, descriptions (NVDA parity)

Date 2026-10-17. Branch `worktree-agent-a3b9b6b013b2bcaf7`, head `f3e9ce3` (code at `f8f17ac`).
Reviewer worktree `.claude/worktrees/m8b-review` (detached). Probes:
`bench/results/e2e/m8b-review/probes/` (`probe_present.rs`, `probe_utterance.rs`,
`m8b-review-probes.patch` = probes + four review scenarios), review pages `tests/web/m8b/review/`.

**Verdict: MERGE AFTER FIXES** (one medium must-fix, M-1; the rest can be follow-ups).

## Gates I ran

| Gate | Result |
|---|---|
| `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings` | clean |
| `cargo test --workspace --no-fail-fast` | 122 binaries, 1983 passed, 0 failed |
| `run_m8.sh m8b-review --quick` (Firefox, whole M8 corpus + BWG/BFB/BHO + 4 review scenarios), load 14-16 at start, 9-12 at end | every M8 and M8b scenario fail 0; PF1 p99 0.40 ms, PF2 p99 0.38 ms; only the reviewer scenarios M8-RINV (2) and M8-RNAMES (3) fail, see below. Results `bench/results/e2e/m8b-review/20261017T030643Z*` |
| `gorca config print-default` | `[browse] description_pause = true` with its doc line; also in `docs/config-reference.md` |
| Trial merge with `main` (37e95f3) | 3 textual conflicts, all additive: `browse/commands.rs` (keep both `web_resync_on_key` from main and `web_overlay`), `present/mod.rs` (resolve to `{Changed, changed, present_change}`: main added `Changed`, the branch made `mixed_word` `pub(super)`, so `mixed_word` must NOT be re-exported or it fails E0364), `docs/decisions.md` (keep both rows). Merged `gorca-core`: clippy clean, 651 + 7 tests pass. |

## Findings (ranked)

### M-1 (medium) Error text for a field is silently lost after the 64th invalid control, and focus never re-reads it
`fetch.rs` step 4b takes the first `MAX_ERROR_TEXTS = 64` invalid controls in document order; focus
speech uses the mirror (`web_overlay`), and `read_node_raw` re-reads only on a state event.
Live: `r_invalid.html` (500 invalid fields before a probe field with `aria-errormessage`):
`RI.focus1` heard "Probe field, edit, invalid entry" with no error text. The cap is the likely
cause; I did not instrument it to prove that. NVDA reads the error message of the focused object
on focus. Fix: when the focused web node is INVALID and has no `error-text`, read it in the
focus path (one node, deadline-bound). Alternatively, rank the capped reads near the
viewport/focus. `RI.focus3` also confirms the documented open item: a changed error message is not
refreshed.

### L-1 (low/medium) Fixed 150 ms sleep in `extras::error_text` on every invalid control with no errormessage relation
The Gecko "first GetRelationSet is empty" retry runs for every engine and every node whose
relation set has no `error-message`. That is the common case: `aria-invalid` without
`aria-errormessage`. `fetch` awaits it before it assembles the block, so every fetch of a subtree
with such a control gets at least 150 ms longer. In `source.rs read_nodes`, each worker is
sequential and the path has no cap, so k invalid nodes cost about k/(lanes*4) × 150 ms per batch.
The sleep also ignores the deadline. Live, I saw focus latency of 75 ms under a 100-field
`aria-invalid` toggle storm (`RI.focus1`). Normal focus is about 2 ms. I did not run an A/B on
`main`, so I have not attributed this. Fix: make the retry a `WebQuirks` row (Gecko only, ADR-0008).
Retry only when the whole set came back empty, once per connection rather than per node. Clamp the
sleep to the deadline.

### L-2 (low) In-band `PAUSE = ". "` collides with content
`strip_pause` and the verbalizer match any segment that starts with ". ". Probe
`probe_pause_prefix_collides_with_content` FAILS: the line ". Dot leading paragraph" becomes "Dot
leading paragraph" when `description_pause=false`. The prefix is also stripped from desktop
utterances (`flow.rs` applies it to every focus presentation). Fix: use a dedicated marker, for
example a `SegmentKind::Description` or a flag on `Segment`, not text.

### L-3 (low) NVDA parity gaps in `takes_authored_name` and the browse line
NVDA `gecko_ia2.cpp:786` (`nameIsContent`) uses link, button, toggle, menuitem, graphic,
**heading**, page tab, buttonmenu, and check/radio when the label is not visible; tree items are
not in that list.
- Heading with `aria-label`: gorca speaks the content (live `RM.h`, unit probe P2 FAIL). NVDA speaks
  the label.
- Check box named only by `aria-label`: the line says "check box not checked" with no name (unit
  probe P1 FAIL). This predates M8b, because `marker_items` does not name check boxes, but it is
  in scope for "names everywhere".
- Tree item with `aria-label`: gorca replaces the content (P4). NVDA keeps it.

### L-4 (low) "not selected" on a focused menu item
`aria::state_words` puts `Role::MenuItem` in `quiet_selected`, so the negative applies as well.
NVDA `_processNegativeStates` excludes MENUITEM. Probe P6 FAILS: `["Bookmarks","menu item","not
selected"]`. This is reachable if Gecko gives SELECTABLE+FOCUSABLE to menu items with a `tag`
(chrome XUL, combobox options mapped to MENU_ITEM). I have not verified that live.

### L-5 (low) State order differs from NVDA for the late-bit states
NVDA sorts by `State` bit value. HASPOPUP_DIALOG/GRID/LIST/TREE (47-50), HALF_PRESSED (45) and ON
(46) come after REQUIRED (15) and INVALID_ENTRY (17). gorca puts "opens dialog" and "on/off"
before "required". Probe P7 heard "collapsed, opens dialog, required". NVDA would say "collapsed,
required, opens dialog". This only matters when those states combine.

### L-6 (low) Hostile-page harness is weak
`HO.fan`, `HO.big` and `HO.h` expect only "button" or nothing, with no `.not()` and no length
bound. A megabyte name read whole would pass. Add `.not(&["AAAAAAAA…"])`-style guards or a max
spoken length.

### Info
- Error-text targets ignore the relation's bus name and use the same `AppConn`. `aria-errormessage`
  is same-document, so this is harmless, but checking the bus is cheap.
- (5) No engine names in core logic. The Gecko/Blink rows live in `extras.rs WebQuirks`, which is
  good, except L-1.
- (3) Desktop: the web branch is gated on a `tag` attribute. GTK and Shell do not expose `tag`
  (grep of refs/gtk, gnome-shell, mutter). The only desktop-visible changes are L-2 and the
  verbalizer joining ". "-prefixed segments with ". ". I did not rerun A1/N1 (implementer:
  a1_wrong 0).
- (6) The Chromium gaps are honest. The 3 `NOT_SELECTED` gap rows have an engine reason. Expected
  strings follow NVDA labels and order (`role.py`, `processAndLabelStates.py`).
- The two intentional deviations are sensible. A combobox with "opens list" silent matches NVDA's
  generic HASPOPUP discard. Silent `selected` for list/tree/menu items matches NVDA
  `_processPositiveStates`. "not checked / not pressed / half checked / half pressed" follow
  `_processNegativeStates` and do not double up with the old M8 words, because `push_states` was
  replaced.
- Hostile vectors that held: 300 buttons described by a 1 MB div (`RD.*` pass, H 0.3 ms, focus
  6 ms), 300 buttons labelled by a 1 MB div (`RN.*` pass), and an icon button labelled by a hidden
  span (`RM.hid` pass).
