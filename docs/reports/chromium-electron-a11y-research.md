# Chromium / Electron accessibility on Linux: why Signal has no browse mode in gorca

Date 2026-10-19, web-a11y-engineer. Status: the cause of the live failure is narrowed down but NOT
reproduced in-loop (see "Unverified"). Probes: `tools/electron-probe/`.

## 1. How Chromium/Electron expose accessibility

Linux: ATK bridge in `refs/chromium/ui/accessibility/platform/ax_platform_node_auralinux.cc`.
* Root web area -> `ATK_ROLE_DOCUMENT_WEB` (l.2975). `role=application` with a parent -> `ATK_ROLE_EMBEDDED`
  (l.2673-2680): it is NOT a document or application role on Linux, so "application mode" signals
  differ from Windows.
* ATK objects exist only if `AXMode::kNativeAPIs` is on (l.2568). Chromium turns it on from the a11y bus
  `IsEnabled` property (atk_util_auralinux.cc l.155) or `--force-renderer-accessibility`; kScreenReader
  only via the /proc "orca" scan (`content/browser/accessibility/browser_accessibility_state_impl_auralinux.cc`).
  The web tree appears after the first `GetAttributes`/`GetRelationSet` (gorca sends it: row `needs_wake_call`).
* Events: `children-changed::add/remove` fire only for the ROOT of a created/deleted subtree
  (`OnSubtreeCreated/OnSubtreeWillBeDeleted`, l.4008-4041). Reorders, ignored-flag flips and many
  `showing` changes are silent. Signal measured below: tree 35 -> 16 nodes with zero events.
* Electron is Chromium: toolkit name "Chromium", app name = binary name (`signal-desktop`), no
  Electron-specific a11y code on Linux; gorca classifies it by toolkit (`Toolkit::classify`).

Windows / NVDA: Chromium exposes IA2 (or UIA). There is NO Signal/Slack app module in `refs/nvda/source/appModules`
(only `code.py`, `teams.py`, `msedgewebview2.py`, `whatsapp_root.py`). Browse mode comes from the generic
`NVDAObjects/IAccessible/chromium.py` `Document._get_treeInterceptorClass` -> `ChromeVBuf` (a Gecko-IA2 virtual
buffer) unless the document is EDITABLE or BUSY. `ia2Web.Document.shouldCreateTreeInterceptor` needs READONLY;
`ia2Web.Application` (ARIA application) never gets one. VS Code is the only special case, and it BLOCKS the buffer
(`appModules/code.py`, performance). NVDA switches to focus mode by focus/role (auto pass-through), not by app.

Orca: `refs/orca/src/orca/scripts/toolkits/Chromium/script.py` only filters caret/children events.
`document_presenter.py` `_is_likely_electron_app` (toolkit chromium and app name not a known browser) enables
STICKY FOCUS MODE for Electron apps by default ("Automatic sticky focus mode for web applications"), i.e. Orca
deliberately does NOT do browse mode in Electron apps. ax_utilities.py knows slack/discord/whatsapp as
background-active Electron apps. So "Electron should support browse mode like NVDA" is true for NVDA only.

## 2. Live observation (read-only, `tools/electron-probe/dump_tree.py`, `focus_info.py`)

Signal 8.31.0, Electron 44.7.2 / Chrome 152, `signal-desktop --force-renderer-accessibility` (pid 3229221).
* application -> frame -> panels (Views UI, menu bar of 5 buttons) -> `document web` (name "Signal", URI
  `file:///opt/Signal/resources/app.asar/background.html`, MimeType text/html, state FOCUSABLE|SHOWING, relation
  EMBEDDED_BY) -> body -> `#app-container` -> nav (tablist: 4 tabs), tabpanel scroll pane, LeftPane, conversation.
* 903 nodes: section 317, static 263, button 140, table row 69, list item 30, article 23, table cell 22, link 5,
  landmark 4, heading 2, entry 1. aria roles seen: grid (`table`), main, navigation, tablist/tab/tabpanel.
  NO role=application, no role=document. A plain DOCUMENT_WEB exists. (Interface lists are unreliable, see memory.)
* Focused node: a `button` inside table row > article > list item > list (conversation list).
* gorca journal (live pid 3160227, started 01:26:44; Signal started 01:36:32; log level info):
  - `phase=create/fetch` at 01:36:34: **nodes=4** (the doc built while Signal was still showing its loading
    state); no other create/fetch afterwards.
  - 312 `web_focus hit=false` and 229 `locate` (2-9 ms) in total, none hit; no WARN about Signal.
  So browse mode never engages: the focus never joins the mirror, and the mirror stayed at its first 4-node state
  (or never learned the later nodes).
* Private Signal (fresh profile, `probe_app.py`, force or poke): tree 4 nodes (t<=1.5 s) -> 35 (t=2.0) -> 16 (t=6.0),
  total events in 10 s: 2 children-changed:add, 1 :remove, 4 accessible-parent, 1 busy, 1 load-complete. The document
  path stays `/accessible/1`. Growth is announced by ONE add event, the shrink by none.

## 3. Why gorca's browse mode does not engage (ranked; not yet proven)

Decision path: focus event -> `web_on_focus` (`crates/gorca-core/src/browse/commands.rs:984`) joins the NodeKey to a
mirrored node (`b.join.get`); miss -> `ensure` (`doc_registry.rs:ensure`) -> `locate` (walk Parent to the topmost
DOCUMENT_WEB, `gorca-doc-source-atspi/src/factory.rs:top_document`) -> `by_root` already exists -> `InDocument`, no
create. Mode is chosen only after a hit.
1. (most likely) The Signal mirror is stale. Built at the 4-node loading state, then the page grows through very few
   events; every later focus misses the join, and nothing ever refreshes the mirror on a miss. Matches live
   numbers (nodes=4, 0/312 hits, locate always resolving to the existing doc). Chromium also changes silently (35->16).
   Fix class: "focus miss inside a known Chromium document" must trigger a targeted refetch (path from focus to the
   document + children lists) or a resync, rate-limited, instead of only `InDocument`.
2. Events do not reach the source or are unmapped: `doc_route.rs` routes by join (unknown node -> the app's
   document), `source.rs::to_atspi` needs the parent in `live.locals`; a big subtree add whose parent was dropped by
   dedupe/coalescing would be lost. Needs `gorca_doc_source_atspi=debug` on the live reader to rule out.
3. Even with a joined mirror, mode policy: focus lands on buttons/grid rows -> `forms::passes_through` -> Focus mode
   (auto), arrows go to Signal's own list navigation. NVDA behaves the same on focus changes, Orca forces sticky focus
   mode. The owner needs "browse mode stays on after focus on a control" / Escape-to-browse for application-like Chromium
   documents (setting, per-app default via the add-on layer).
4. (unlikely) role=application / dormant / document replacement: none observed in Signal.

## 4. Chrome vs Debian Chromium vs Electron (private headless session, same M8-QN quick-nav steps)

System change: added `/etc/apt/keyrings/google-chrome.gpg`, `/etc/apt/sources.list.d/google-chrome.list`
(dl.google.com stable main) and installed `google-chrome-stable` 155.0.8059.84 (/opt/google/chrome). Also npm-installed
Electron 44.9.0 (Chrome 152 like Signal) under `~/.cache/electron-probe` (mode 700); remove with `rm -rf` of that dir.
Rig: `tests/web/m9/run_m9.sh m8 <label> --only M8-QN --repeats 1` with `M9_CHROMIUM` = chrome / chromium / the
`tools/electron-probe/electron-wrapper.sh` (tiny Electron app, binary renamed `chromelectron` so the rig's name
filter matches). Results in `bench/results/e2e/el-*`.
* Heading nav (H, Shift+H) and button nav (B) PASS identically on Chrome 155, Debian Chromium 154 and Electron 44
  (steps K/hl failures are cascades of the step subset filter, the same in all three). So generic Electron +
  gorca browse mode works; a page that grows 3 s after load also works (`delay-wrapper.sh`, 3000 ms, Chrome and Electron).
* NOT reproduced: the Signal case (early 4-node build, then growth). Variants with longer delays made the rig's
  `find_doc` fail (60 s, cause unknown), and Signal itself in the rig fails at `GrabFocus` (headless window not active).

## 5. Where fixes belong (ADR-0022 single semantic model / M18a add-ons)

Generic Chromium-engine base (connector + doc-source quirk rows, all Chromium browsers and Electron):
* focus-miss resync / targeted fetch (item 1), growth-by-single-add handling, silent-change Revalidate on reading steps;
* "web app" detection from structure (no landmarks/headings vs many controls, grid/tree/list roles) feeding the
  mode policy, not from the app name;
* document-ready/busy handling: do not treat the first small tree as final; re-check on `busy` state and load-complete.
Per-app extension (add-on): Signal/Slack/Telegram/VS Code defaults (start in focus or browse mode, custom virtualized
list navigation, message-list read-out rules, VS Code: no buffer at all, like NVDA), keyed by app name/URI
(`background.html`) through the add-on selector layer, never core special cases.

Minimal fix set for Signal browse arrows: (a) refresh/rebuild the mirror when a focus misses a known Chromium document
(and on the first `ChildrenChanged:add` after a tiny initial build); (b) verify live with `RUST_LOG=gorca_doc_source_atspi=debug`;
(c) a per-app/structure policy so browse mode can be entered and held on a web-app document (Escape / mode toggle), with
Signal default decided by the owner.

## Unverified / next steps
* No in-loop reproduction of Signal; item 1 is inferred from live numbers plus the private tree timeline.
* Next: run the live reader with debug for `gorca_doc_source_atspi`/`doc_actor` (needs a restart: owner decision), or build a
  Signal-like test page (loading shell + one big subtree add + silent shrink) in `tests/web/` and a rig variant that
  creates the document before growth.
