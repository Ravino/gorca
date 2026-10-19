# Signal (Electron) chat navigation: how Orca does it, what gorca lacks

Date 2026-10-19, web-a11y-engineer. Research only, no product code. Cross-reference:
`virtual-list-navigation-math.md` (math-researcher: browse mirror over virtualized lists, F1-F3, detector, identity),
`chromium-electron-a11y-research.md`, `chr0-implementation.md`, decisions 2026-10-19 (DP-3, ADR-0016 Q-P1..6).

Raw material: `docs/reports/signal-orca-vs-gorca/` (one `run-*.txt` per rig run, `live-signal-tree-structure.txt`).
Rig: `tests/web/chr0/signal_cmp/` (`run.sh READER APP SCENARIO OUT`, `matrix.sh`, `driver.py`, `show.py`),
fixture `tests/web/chr0/pages/signal-like.html`, read-only live tools `tools/electron-probe/struct_dump.py`,
`event_watch.py`.

## 0. Summary

1. **Two different Orcas.** `refs/orca` is Orca 51.1 (has "sticky focus mode for Electron apps"). The installed
   `/usr/bin/orca` is **48.1**, which has no Electron logic at all. All Orca runs below are 48.1; 51.1 was read, not run.
2. After CHR-0, gorca's remaining Signal failures are NOT the mirror. Reproduced in a private session on a
   Signal-shaped page (Electron and Chromium, same result):
   * **Message list**: arrows reach the app and focus follows, but gorca speaks the same wrong phrase for every
     message: "React to message, row" (Orca: "Contact 0 Message number 398 body text 10:38 Message"). The cause is
     the shallow name-from-content (S3) of an unnamed focused row; it happens with browse mode off as well.
   * **Conversation list** (ARIA grid of buttons): gorca stays in browse mode, **consumes every arrow** and reads the
     page text line by line; the DOM focus never moves, so Signal's own list navigation never runs. Orca enters
     focus mode on it (grid descendant), arrows pass through, each conversation is spoken as focused.
   * gorca+Space (DP-3 sticky focus mode) fixes the conversation list (verified); it does not fix the message text.
3. Orca's mechanism is simple: it never consumes arrows where the app owns the keys and speaks every `focused`
   event with a content-aware generator. There is no Signal/Electron script in Orca; 51.1 only makes sticky focus
   mode the default for "Chromium toolkit and app name not a browser".

## 1. How Orca works with Signal (source)

All paths relative to `refs/orca/src/orca/` (51.1) unless stated.

* **No Electron/Signal script.** `scripts/toolkits/Chromium/script.py` only filters caret/children/selected events
  and ignores a document without URI. Toolkit name "Chromium" selects it for every Chromium app.
* **Sticky focus mode for Electron (51.1 only)**: `document_presenter.py:175-190` `_is_likely_electron_app`
  (toolkit `chromium` and app name has none of brave/chromium/edge/chrome/opera/vivaldi); used at `:717-724` when the
  focus enters a document, if `auto sticky focus mode for web apps` is on (default True, `:850-861`) and the user has
  not toggled the mode in this app. `enable_sticky_focus_mode` (`:550-580`) sets `in_focus_mode`+`focus_mode_is_sticky`
  and calls `suspend_navigators` (`:366`): caret and structural navigation are suspended, so Orca grabs no arrow or
  letter keys and the page owns them. Also `_is_top_level_web_app` (`:192-215`, used at `:723`): an embedded document with an http(s)
  URI gets the same treatment. The user can leave it with the mode toggle (`user_has_overridden...`).
* **Orca 48.1 (installed)**: `scripts/web/script.py:735` `useFocusMode` and `script_utilities.py:1689`
  `isFocusModeWidget`: auto focus mode for editable/expandable, combo/list box/menu/tab/tree roles, **table cells and
  tables that are not layout (`TABLE_CELL`, `TABLE`), grid descendants**, listbox items, button with popup, toolbar
  descendants. A table row (Signal message) is not a widget: "Not using focus mode ... due to lack of cause".
  Insert+A is a temporary focus mode that falls back to browse mode when the focus moves to a non-widget
  (observed: "Browse mode" spoken while arrowing through messages); only a double press is sticky.
* **Speech follows the app's focus**, not a virtual cursor: `scripts/web/script.py:1266` `_on_focused_changed`
  (document focus ignored, `set_locus_of_focus`), `:433` `locus_of_focus_changed` -> `present_object(new_focus)`, with
  `interrupt_if_needed_for_focus_change` first. In focus mode caret events set the locus too (`:800`).
  The generator speaks the **contents of the focused object**: a message row is spoken from its text descendants.
* **No arrow handling of its own in a chat**: no table or list navigation is involved (Signal's Timeline handles
  ArrowUp/ArrowDown, see section 2); live regions go through `live_region_presenter.py` (politeness queue, history
  commands). Container changes are announced on focus moves ("leaving list.", "leaving main content.").
* Measured (rig, Orca 48.1, host load 10-14): Orca is slow, typically 1-3 s per focus speech, which shifted step
  attribution by one. gorca: key to speech start 3-23 ms in focus mode. Orca latency was not benchmarked.

## 2. Live Signal 8.31 (read-only; names, text and contacts are redacted)

Tools: `struct_dump.py` (lengths and keys only), `event_watch.py` (registryd listener, no actions), the running
client's renderer bundle in `app.asar` (code, no user data) and gorca's own journal (`gorca_phase`).

* Tree (`live-signal-tree-structure.txt`): application `signal-desktop`, toolkit "Chromium" -> frame (`embeds`) ->
  Views panels (menu bar of 5 buttons) -> `document web` (name "Signal", `file:///opt/Signal/.../background.html`,
  `embedded-by`) -> nav (tablist, 4 tabs) -> tabpanel (heading h1 `live=assertive`, search entry, toggle button) ->
  `presentation[aria-live=polite]` -> **`grid` "Conversations"** > `div[row]` (22 children) > `span[row]` >
  `span[gridcell]` > `button` (name 80-140 chars, 3-4 children). No role=application, no role=document, 700-900 nodes.
* Message list (from the bundle; no conversation was open while observing, so it was not sampled live):
  `div[role=presentation][tabindex=-1][onKeyDown]` (Timeline) > `main` > `div[role=list]` >
  `div[role=listitem][data-message-id]` > `div[role=article][aria-labelledby=message-accessibility-contents:ID]
  [aria-roledescription]` > `div.module-message[role=row][tabindex=0]` (**the focus target; every message is
  tabbable, no roving tabindex**) > ... `div#message-accessibility-contents:ID[role=row][tabindex=-1]` (text) + buttons.
  The whole chain sits under `ConversationView__timeline[aria-live=polite]`.
* Keys: the Timeline's keydown handles ArrowUp/ArrowDown (and PageUp/PageDown) itself: it moves the "targeted
  message", re-renders the window and focuses the neighbour's row (`preventDefault`). So arrows in a chat are the
  app's own roving *by script*, delivered to the reader only as `state-changed:focused` plus children-changed at the
  window edges. The conversation list moves focus between its buttons the same way.
* gorca live journal since the reader's restart 21:44 (binary with CHR-0): 112 `web_focus`, **109 `hit=false`**,
  3 hits; 38 repairs (13 `focus_miss`, 15 `orphans`, 8 `young`, 2 `root_state`), each repair `changed=false` with
  361-725 nodes. Caveat: `web_focus` is logged for every web app (Firefox docs were mirrored too), so these misses
  are NOT attributed to Signal. The passive listener captured no focus or caret events (owner idle), only the
  per-minute `time` text updates.

## 3. Private-session reproduction (`tests/web/chr0/signal_cmp`)

Setup: private compositor + isolated AT-SPI session (M7 pattern), Electron 44 app renamed `signal-desktop` (hard
link) and Debian Chromium 154, `--force-renderer-accessibility` for both readers, real keys through the emulated
Mutter (consumed keys never reach the page) and the private RemoteDesktop keyboard. Readers: Orca 48.1 and
`<repo>/target/release/gorca` (main d018080c, not rebuilt); speech from a mock speech-dispatcher / Orca debug log.
Fixture structure: section 2 (grid list + virtualized Timeline with edge-only DOM changes, 400 messages, 14 in DOM).

| Case | Orca 48.1 | gorca main |
|---|---|---|
| list, Down x3, Up x2 | focus mode on arrival, arrows pass (`consumed=False`), focus moves, speaks "Contact 1, last message of contact 1, 10:01 button" each | "browse mode", arrows **consumed** x5, focus stays on contact 0, speaks "last message of contact 0", "button, Contact 1, ..." (page lines) |
| list + gorca+Space first | n/a | "focus mode", arrows pass, "Contact 1, last message ..., button" per key (3-8 ms to speech) |
| chat, Up x4, Down x2 | arrows pass, speaks "Contact 0 Message number 398 body text 10:38 Message" (author, text, time, role description) | arrows pass (auto focus mode), speaks "React to message, row" for **every** message |
| chat, `browse.enabled=false` | n/a | identical "React to message, row": generic path, not browse |
| chat, Home/End | window swap, focus follows, speaks message | same, plus mirror miss -> `focus_miss` repair (4 of 11 focus events missed, all on freshly created nodes) |
| chat, Tab to composer | "leaving list. leaving main content. Send a message entry. Focus mode" | "Send a message, edit", then "blank" (no container context) |
| gorca+Space then arrows in chat | Insert+A: focus mode, falls back to "Browse mode" at the first non-widget focus; arrows then consumed (caret nav) | "browse mode" (cursor reads text lines "React to message, button", "Message number 398 body text"), second toggle "focus mode" + a correct "Contact 0 Message number 398 body text, row" |

Chromium and Electron gave identical results. Raw tables: `docs/reports/signal-orca-vs-gorca/run-*.txt`.

**Rig limitation (honest):** in the default runs Orca 48.1 never consumed a key, even on a plain article page
(`oe_doc`: "h" is echoed, navigators log `Suspended: True ... in document content: False` from script activation
and are not refreshed). After Insert+A its grabs did work (`consumed=True`), so the grab path is fine; the default
suspension is Orca 48.1 behaviour in this rig whose cause I did not find. So Orca's browse-mode arrows were not
observed; what the table shows for Orca is its focus-follow speech and its (non-)consumption.

## 4. Gap table

Generic = any web app or engine, belongs to core/connector. App-specific = a profile row or add-on (ADR-0016,
M18a hooks), never a core special case. Priority P0 = makes the owner's chat unusable.

| # | Behaviour | Orca mechanism | gorca today (evidence) | Kind | Proposed home | Pri |
|---|---|---|---|---|---|---|
| G1 | Speak a focused container with no accessible name (row, listitem, article, group) from its text content | content generator over descendants (`speak_contents`, `present_object`) | S3 name-from-content (`gorca-fuse/src/taint.rs:80`) joins only the **children's final names**: the nameless content row contributes nothing, the button "React to message" is the only named child -> "React to message, row" for every message (miss_reply snapshot name in `present.log`; same with browse off). The mirror's full text (`subtree_label`, `browse/commands.rs:614`) is skipped because the derived name is non-blank | generic | fuse S3: recurse accname 1.2 step 2F through nameless containers (text of the subtree, bounded); keep `web_overlay` fill authoritative for Row/ListItem/Article; test with `signal-like.html` | P0 |
| G2 | Role description / list position on a message | speaks the ancestor article's `aria-roledescription` ("Message"); role word otherwise | says the role "row"; `roledescription` attribute is carried by the mirror but not used for a focused descendant | generic | presentation: nearest ancestor roledescription for row/group roles inside article/listitem (to verify the exact Orca rule) | P1 |
| G3 | Composite widgets made of buttons/cells (ARIA grid, listbox, toolbar, tree) own their arrows | `isFocusModeWidget`: grid descendant, table cell/table, listbox item, toolbar descendant (`script_utilities.py:1689`, 51.1 `document_presenter.py:424-510`) | `forms::passes_through` (`browse/forms.rs:23`): Row/Cell focusable yes; **Button inside a gridcell no**, Table needs a widget role. Conversation list stays in browse: arrows consumed 5/5, DOM focus never moves | generic | core `forms.rs` + mirror ancestors: focus inside `xml-roles=grid/treegrid/listbox/toolbar/menu` -> auto focus mode (NVDA does the same for grid cells) | P0 |
| G4 | "Web app" default = focus mode | 51.1 `_is_likely_electron_app` + sticky focus (`document_presenter.py:175,717,850`); 48.1 none | `[browse] mode_default` exists in the schema but is "Not applied yet" (`gorca-config/src/settings.rs:1292`); DP-3 toggle works (`list_focus` run) but is manual per document | split | (a) core: apply `mode_default` and the toggle scope (per document/app, Q-P3); (b) connector: structure-based "web app" detector (math report section 4), not app names; (c) `profile row gorca/signal`: `[browse] mode_default = "focus"` for app `signal-desktop` (first-party row, on automatically, PF-5) | P0 |
| G5 | Focus on nodes created by window swaps (Home/End, edge loads) | no mirror, speaks the focused event | 4 of 11 rig focus events missed (all fresh nodes); `Loading` (grabs off) for 1 s, then **Browse at the document root with arrows grabbed** (`web_loading_to_browse`, `browse/commands.rs:2219`): a later arrow reads page text instead of reaching the app; CHR-0 repair follows (`focus_miss`, `orphans`) | generic | core doc_actor: on a join miss inside a known web-app document, stay in focus-follow (no grab) until the repair resolves; never fall to Browse@root while the focus is real; math report A2 (identity cursor, edge prefetch) | P1 |
| G6 | Container context on focus moves | "leaving list.", "leaving main content.", "Send a message entry." | focus mode path says only "Send a message, edit" and "blank" | generic | presentation: ancestor diff for focus moves in web documents (web_container_tail exists for browse) | P2 |
| G7 | Live regions under virtualization | politeness queue, per-region history commands (`live_region_presenter.py`) | the Timeline is `aria-live=polite`; Home/End logged two `live_msg` of the whole 14-message block (not spoken within 4 s here). Policy for list-window re-renders is undefined | generic | doc_actor live policy: ignore additions that are window re-renders of a virtualized list (detector); coalesce | P2 |
| G8 | Mode toggle semantics | Insert+A temporary, double = sticky; sticky auto-set for Electron, user override remembered | gorca+Space is sticky until toggled (good); no per-app memory | profile | ADR-0016 Q-P3/Q-P4: "save mode" chord writes `mode_default` to the app profile | P2 |
| G9 | Signal's quirks (Timeline keydown, tabbable rows, `data-supertab`, `aria-roledescription` on article, labelledby chain) | none (generic) | none | app-specific | add-on / profile row only if G1-G4 leave a residue: e.g. message-reading template (author, text, time) keyed on `data-message-id` | P3 |

What is NOT a gap: the arrows themselves (no consumption in focus mode), speech latency (3-23 ms), the mirror
freshness after load (CHR-0), Electron detection by toolkit (the connector already classifies Chromium).

## 5. Recommendation

1. P0 now, generic: **G1** (fuse S3 text recursion, small, testable with the fixture) and **G3** (grid/listbox/
   toolbar ancestors -> auto focus mode). Together they make Signal usable with arrows on default settings.
2. P0 with the owner's DP-3 decision: **G4** apply `mode_default` and add the `gorca/signal` first-party row
   (`app name signal-desktop` -> focus). Treat "Orca does it by toolkit" (51.1) as a hint for a structure-based
   default, not as a rule to copy (it would also put Slack/Discord/VS Code-like apps in focus mode).
3. P1: G5 (no Browse@root fallback on a join miss in a live document), G2.
4. Keep the app-specific layer empty until G1-G4 are in; then re-run `tests/web/chr0/signal_cmp/matrix.sh`.

## 6. Unverified / open questions

* Which Orca the owner compared with: 48.1 (installed) has no sticky mode; 51.1 would explain "fine" by design. In my
  rig Orca 48.1 also passed all arrows (cause of its suspended navigators not found).
* No conversation was open in the live Signal, so its message rows are known from the renderer bundle, not sampled.
  The 109 live `hit=false` cannot be attributed to Signal (needs `gorca_doc_source_atspi=debug` or an app field
  in `web_focus`).
* Orca 51.1 was not executed (needs a meson build and schemas); its behaviour is from source.
* The fixture imitates structure, not Signal's React timing; the real list recycles nodes differently.
* Orca speech timing is from a loaded host; attribution of Orca lines to steps is off by one in places.
