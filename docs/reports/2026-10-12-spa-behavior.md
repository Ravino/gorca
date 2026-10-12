# SPA behaviour over AT-SPI: Chromium 153 and Firefox 153 ESR

Author: web-a11y-engineer, 2026-10-12. Input for the mirror's diff and identity-matching design (R6, decision W/G).
Corpus: `tests/web/spa/` (README there). Raw results: `tests/web/spa/results/{chrome,firefox}/` (per-scenario summary JSON plus gzipped raw
event streams). Real trees: `research/web-graph/real-trees/` (46 dumps, 44k nodes, README with the format).
Everything was measured on this host (Debian 13, GNOME Shell 48.7, at-spi2-core 2.56) in private instances; the user's browsers and buses were never touched.

## 1. What was built and how it was measured

* **Apps** (static esbuild bundles, one server `node serve.mjs`): React 19 (full: route change with/without focus management, keyed/unkeyed list,
  react-window 10k feed, infinite scroll, modal with focus trap, live-region floods, validated form, ARIA 1.2 combobox, tabs, 100x6 sortable grid),
  and Vue 3 + vue-router, Angular 20 (JIT, zone.js), Svelte 5 for route change (x2 focus modes), keyed/unkeyed list and a 10k virtualized feed.
  The four apps share one DOM contract, so one driver script drives all of them. React's windowing is `react-window` 1.8; the other three use a
  hand-rolled equivalent (absolutely positioned rows in a tall spacer, overscan 5) - same DOM shape, not the same library.
* **Driver**: trusted input (Chromium CDP `Input.*`, Firefox Marionette actions), raw AT-SPI signals captured on the a11y bus with the
  *sender object path* (so identity is observable), per-step DOM MutationObserver counts, before/after tree snapshots of the document subtree
  (5-6 sequential calls per node) and identity metrics. 118 reported steps per browser over 27 scenarios + 3 static large pages.
* **Chromium**: headless=new, `--force-renderer-accessibility`, no window. **Firefox**: needs a Wayland compositor for its ATK bridge.
  See section 7 for what went wrong with the user's compositor and the fix (a PRIVATE headless gnome-shell). Final data: no focus was stolen
  (real active app before: Firefox, after: gedit, i.e. the user's own changes); the earlier headed runs on the user's session were discarded.

## 2. Event stream characteristics (118 steps per browser, SPA scenarios)

| | Chromium | Firefox |
|---|---|---|
| first event after trusted input, p50 / p95 / max | 6.5 / 30 / 56 ms | 11 / 39 / 57 ms |
| last event of the burst, p50 / p95 | 19 / 288 ms | 33 / 357 ms |
| timer-driven floods | last event = end of the flood (2.06 s, 3.1 s) | same (2.05 s, 3.0 s) |
| burst never settled within the cap | 0 | 0 |
| events by kind (all steps) | CC:add 742, PC:name 599, CC:remove 598, TC:insert 201, focused 158, TC:delete 65 | CC:add 1674, CC:remove 1449, TC:delete 946, TC:insert 942, PC:name 718, visited 194, Focus 106 |

`CC` = `ChildrenChanged`, `TC` = `TextChanged`, `PC` = `PropertyChange`. Firefox appends `/system` to the kind of events it generates for
non-user DOM changes (`add/system`) - match on the prefix.

Per user action (React; the other frameworks within a few events, see the tables in section 3):

| action | Chromium | Firefox |
|---|---|---|
| route change (swap `<main>`, ~90 nodes) | 10-18 events (3-4 `remove`, 2-5 `add`, 3-5 name changes, 2 focus) | 22-40 events |
| list: prepend 1 / edit 1 | 3 / 3 | 5 / 5 |
| list unkeyed prepend or reverse (50 rows rewritten in place) | 52-56 (all `PropertyChange:name`) | 152-154 (name + text delete + text insert per row) |
| list keyed reverse (50 DOM moves) | **2 (focus only)** | 104 (50 remove + 50 add) |
| grid sort, 100 rows, keyed | 3 (`RowReordered` x1) | 201-287 (94-99 remove + 94-99 add) |
| 100 renames on a static page | 100 | 500 (remove+add of the text child + text delete+insert per node) |
| virtualized feed, one scroll step | 3-39 (1 event per row added/removed) | 4-42 |
| virtualized feed, `End` (scroll animation, many frames) | 65-73 | 272-377 |
| live chat flood, 100 messages / 2 s | 215 (100 `CC:add` + 113 `TC:insert`) | 202 (same pair, `/system`) |
| live ticker, 30 updates / 3 s | 62 (20 name, 20 text-delete, 20 text-insert) | 60 |
| modal open | 5 | 10 |
| tab switch (panel content replaced in place) | 13 | 19 |

Ordering inside a burst (Chromium route change): name changes first, then `ChildrenChanged:remove` events (child reference = the removed
object's path, in *non-index order*), then focus, then `ChildrenChanged:add`. Firefox interleaves `remove`/`add`/`TextChanged:*/system`
and emits `StateChanged:visited` for links it re-creates. In both browsers a `remove` event only names the **top** of a removed subtree
(a 20 943-node route swap on the 10k-element page = 874 `remove` events, one per top-level child, and nothing for the descendants).

Event payload facts: Chromium `ChildrenChanged` always carries index and child path (2815/2815). Firefox: 838 of 6704 have index -1 (append) and
108 carry a null child (`/null`), so the mirror must be able to treat any `ChildrenChanged` as "refetch the parent's child list".

**No live-region signal exists in either browser.** A flood reaches the client only as `ChildrenChanged:add` + `TextChanged:insert` (with the
inserted text in `any_data`) on nodes under an element whose attributes carry `live`/`container-live`/`atomic`/`relevant`
(both browsers expose them: `container-live=polite`, `atomic=true`, `relevant=additions text` on Chromium). The live-region policy therefore has to be
derived from the mirror (container-live ancestor + insert events); 30 DOM ticker updates produced 20 text events (coalesced by the browser).

## 3. Identity: what survives a re-render

Metric definitions (`tests/web/spa/driver/analyze.py`): *common paths* = AT-SPI object paths present before and after; *gone/new* = paths only
before/after; *id kept* = among DOM `id` values visible before and after, how many are still on the same object path.

### Lists (50 rows; `-gone/+new` counts AT-SPI nodes incl. text leaves; all four frameworks, summary of the table below)

| scenario | Chromium | Firefox |
|---|---|---|
| keyed, reverse (50 DOM moves) | paths all stable, **no event** (silent) | 100 gone/100 new, DOM id kept on same path 12/62 |
| keyed, prepend / remove / edit | 1 add / 1 remove / 1 name event; paths stable | same shape, +text events; paths stable |
| unkeyed (React, Svelte, Angular), prepend/reverse | 50 name events; **paths stable, objects now mean different items**; `id` attribute stale (see below) | 50 objects keep path, `id` attribute updated; 150 events |
| unkeyed (Vue), prepend/reverse | 50 remove + 50 add (text children recreated), 50 `id` attributes updated | 250 events |
| virtualized feed, scroll | rows that stay in the window keep path (8/8, 19/19...); entering rows are new objects | same |
| route change | shell (header/nav/footer, ~10-14 nodes) keeps paths; `<main>` subtree all new | same (shell 10) |

Full per-step table (`ev` = events, `-g/+n` = gone/new nodes, `id` = DOM id kept on same path / ids present):

| fw | variant | action | Chromium | Firefox |
|---|---|---|---|---|
| react | keyed | prepend | 3 ev, -0/+3, id 61/61 | 5 ev, -0/+2, id 61/61 |
| react | keyed | reverse | 2 ev, -0/+0, id 62/62 **silent** | 104 ev, -100/+100, id 12/62 |
| react | keyed | remove middle | 3 ev, -3/+0, id 61/61 | 4 ev, -2/+0, id 61/61 |
| react | keyed | edit item 10 | 3 ev, -0/+0, id 61/61 | 5 ev, -0/+0, id 61/61 |
| react | unkeyed | prepend | 53 ev, -0/+3, id 60/61 | 154 ev, -0/+2, id 11/61 |
| react | unkeyed | reverse | 56 ev, -0/+0, id 61/61 | 152 ev, -0/+0, id 12/62 |
| react | unkeyed | remove middle | 28 ev, -3/+0, id 60/61 | 81 ev, -2/+0, id 36/61 |
| react | unkeyed | edit item 10 | 3 ev, -0/+0, id 61/61 | 5 ev, -0/+0, id 61/61 |
| vue | keyed | prepend | 3 ev, -0/+3, id 61/61 | 5 ev, -0/+2, id 61/61 |
| vue | keyed | reverse | 2 ev, -0/+0, id 62/62 **silent** | 104 ev, -100/+100, id 12/62 |
| vue | keyed | remove middle | 3 ev, -3/+0, id 61/61 | 4 ev, -2/+0, id 61/61 |
| vue | keyed | edit item 10 | 4 ev, -1/+1, id 61/61 | 7 ev, -0/+0, id 61/61 |
| vue | unkeyed | prepend | 103 ev, -50/+53, id 11/61 | 254 ev, -0/+2, id 11/61 |
| vue | unkeyed | reverse | 102 ev, -50/+50, id 12/62 | 252 ev, -0/+0, id 12/62 |
| vue | unkeyed | remove middle | 53 ev, -28/+25, id 36/61 | 129 ev, -2/+0, id 36/61 |
| vue | unkeyed | edit item 10 | 4 ev, -1/+1, id 61/61 | 7 ev, -0/+0, id 61/61 |
| angular | keyed | prepend | 3 ev, -0/+3, id 60/60 | 5 ev, -0/+2, id 60/60 |
| angular | keyed | reverse | 2 ev, -0/+0, id 61/61 **silent** | 154 ev, -100/+100, id 11/61 |
| angular | keyed | remove middle | 3 ev, -3/+0, id 60/60 | 4 ev, -2/+0, id 60/60 |
| angular | keyed | edit item 10 | 3 ev, -0/+0, id 60/60 | 5 ev, -0/+0, id 60/60 |
| angular | unkeyed | prepend | 53 ev, -0/+3, id 59/60 | 154 ev, -0/+2, id 10/60 |
| angular | unkeyed | reverse | 52 ev, -0/+0, id 60/60 | 152 ev, -0/+0, id 11/61 |
| angular | unkeyed | remove middle | 28 ev, -3/+0, id 59/60 | 79 ev, -2/+0, id 35/60 |
| angular | unkeyed | edit item 10 | 3 ev, -0/+0, id 60/60 | 5 ev, -0/+0, id 60/60 |
| svelte | keyed | prepend | 3 ev, -0/+3, id 61/61 | 5 ev, -0/+2, id 61/61 |
| svelte | keyed | reverse | 2 ev, -0/+0, id 62/62 **silent** | 106 ev, -100/+100, id 12/62 |
| svelte | keyed | remove middle | 3 ev, -3/+0, id 61/61 | 4 ev, -2/+0, id 61/61 |
| svelte | keyed | edit item 10 | 3 ev, -0/+0, id 61/61 | 5 ev, -0/+0, id 61/61 |
| svelte | unkeyed | prepend | 53 ev, -0/+3, id 60/61 | 154 ev, -0/+2, id 11/61 |
| svelte | unkeyed | reverse | 52 ev, -0/+0, id 61/61 | 152 ev, -0/+0, id 12/62 |
| svelte | unkeyed | remove middle | 28 ev, -3/+0, id 60/61 | 79 ev, -2/+0, id 36/61 |
| svelte | unkeyed | edit item 10 | 3 ev, -0/+0, id 61/61 | 5 ev, -0/+0, id 61/61 |

Reading the tables:

* **The four frameworks are indistinguishable at the AT-SPI level for the same DOM operation.** What differs is the application's key discipline,
  not the framework: keyed vs unkeyed and DOM moves vs in-place rewrites. Vue's unkeyed `v-for` is the one framework-specific case (it recreates
  the text child where React/Svelte/Angular only change the text), and Vue's keyed "edit" also recreates one text node in both browsers.
* **Chromium keeps object identity across DOM moves but does not say they moved.** Reversing a keyed list changes the order of 50 children with
  zero ChildrenChanged events (4/4 frameworks; grids get a single `RowReordered`). A mirror that only applies events keeps the old order forever.
  No event fires for `showing`/`visible` flips either (not measured per state; see risks).
* **Firefox does not keep identity across DOM moves**: every moved subtree is destroyed and recreated (keyed grid sort: 658 nodes gone, 658 new;
  keyed list reverse: 100/100) - but it is loud about it (remove + add events), so the diff can see it and re-match.
* **DOM `id` is exposed by both browsers** (`id`, `tag`, plus `class`, `xml-roles`, `display`, `level`, live attributes) - 3-5 attributes per
  element, no custom `data-*`. It is the only cross-churn key that survives a Firefox re-creation (12/62 kept the path, 62/62 kept the id).
  **But Chromium's `id` attribute goes stale**: after React rewrote the `id` of 50 `<li>` in place (unkeyed prepend), the AT-SPI object still
  reported `item-1` while the DOM said `item-51` (`staleid` experiment, same objects, 1.5 s later). Name changes and children changes
  refresh the node; a pure attribute change does not. Firefox updates the attribute. Real pages rarely put ids on repeated items: on the
  generated large pages 3 of 2005 and 7 of 10 233 nodes carry an id, so an id-only identity scheme would not work in general.
* AT-SPI paths are per-process counters in both browsers (Chromium: AX node ids `/org/a11y/atspi/accessible/N`; Firefox: bridge ids). No stable
  cross-run identity exists (neither browser implements `AccessibleId`, as established earlier). I did not observe a path being reused for an
  unrelated node within a run; in-place identity swaps (same path, different logical item) are the real hazard, see unkeyed rows.

## 4. Route changes and focus

* 5 navigations per scenario (link click, link to detail, back link, nav link, `history.back()`), x2 focus modes, x4 frameworks, x2 browsers.
  Mean events per navigation: Chromium 12-14 (no focus management) / 15-18 (with); Firefox 26-29 / 31-36. Settled in 14-31 ms mean on Chromium
  (React with the announcer: 165 ms because the announcer text is a second render), 66-155 ms on Firefox.
* Shape: shell survives with stable paths; `<main>` content is a whole-subtree swap announced by 3-4 `remove` + 2-5 `add` events; the
  document's accessible name changes (`PropertyChange:accessible-name` on the document, the page title) every time, even without focus management.
* **Without focus management** the focused link is destroyed (detail/back clicks): DOM focus falls to `<body>`; Chromium sends
  `focused` on the *document* (name = new title); Firefox first sends a (stale) focus for the removed link, then the document.
  When the clicked nav link survives (Products, About) focus stays on it and the only user-visible signal is the title/name change and the
  subtree swap. `history.back()` produced no focus event in either browser.
* **With focus management** (focus moved to `h1[tabindex=-1]`, `role=status` announcer): both browsers send `focused` on the heading with its new
  name (`heading#page-title[Products]`) in the same burst as the swap; Firefox additionally reports the stale link focus first.
  The announcer text arrives as normal `TextChanged:insert` events inside a `status` element (no special signal).
* Design consequence: after a route swap the mirror must (a) treat the 3-4 removes as subtree drops (their descendants are never evented),
  (b) fetch the added subtrees, (c) re-anchor the virtual cursor: if the cursor node was dropped, land on the focus event target (heading) when
  there is one, else on the document start/title.

## 5. Other widget scenarios (React)

* **Infinite scroll**: each load = ~21 `ChildrenChanged:add` + 2 `busy` state changes (aria-busy true/false) + 1 remove (the "Loading" text) in
  <= 290 ms; existing items keep paths 100 %. Incremental append, cheap to mirror.
* **Modal with focus trap + inert background**: open = `ChildrenChanged:add` of the dialog subtree + `focused` on the first input (+ caret moved);
  Tab trap = exactly one focus pair per key press; Escape = one `remove` + `focused` back on the opener. Firefox additionally sends 4
  `PropertyChange:accessible-parent` events on open. `inert` produced no per-node events on the background in either browser (tree states of
  the background nodes were not diffed for this).
* **Live floods**: 100 messages / 2 s = 100 add + 100-113 text-insert events, max 17 events / 100 ms (Chromium); a ticker (atomic) = per update one
  name change, one text-delete and one text-insert on the same object. See section 2 for the live-region consequence.
* **Form validation**: blur with error = `invalid-entry` state change, `PropertyChange:accessible-description` (aria-describedby target text
  resolved by the browser), 1-3 `ChildrenChanged` for the error span, `accessible-parent` changes (Chromium 3-6, relabeling of the label/input
  relationship); submit with summary `role=alert` = 4 add + 3 remove, focus moves to the first invalid input. No dedicated "alert" event: it is a
  `ChildrenChanged:add` of a node whose role is `alert`.
* **Combobox / aria-activedescendant**: arrow key = `focused` on the **option** object (Chromium: `focused` 2 events + `selected` + `SelectionChanged`;
  Firefox: `Focus.Focus` + `focused`). DOM focus stays on the input but AT-SPI focus follows activedescendant, i.e. the browser performs
  the remapping; the mirror just follows `focused`. Typing a 2nd character removes 17 options (17 `remove` events, one per option), no batch.
* **Tabs**: arrow key = `selected` on the two tabs, `SelectionChanged`, and the panel is rewritten in place: 8 name changes on Chromium (paths
  stable, content swapped: an in-place identity swap), 6 text delete/insert pairs on Firefox.
* **Grid**: arrow navigation = focus pair only; sort = Chromium 1 `RowReordered`, Firefox 200-290 events and 658 recreated nodes.

## 6. Large static pages (tests/web/pages generator; mutation helpers `mut.*`)

2000- and 10 000-element pages (Chromium 4168 / 21 203 AT-SPI nodes, Firefox 2005 / 10 233 - Firefox has no leaf text nodes). Chromium:
`rename(100)` = 100 name events; `addCards(50)` = 50 `add`; `removeCards(20)` = 20 `remove` (500 nodes gone); SPA-style route swap of the
whole page = 874 `remove` events for 20 943 nodes in 155 ms (28 ms on the 4k page). Firefox: `rename(100)` = 500 events, route swap = 889 events
in 101 ms. Harness snapshot cost (sequential bus calls, 5 per node without bounds): Chromium 21k nodes 25.5 s, Firefox 10k nodes 11.7 s (harness only,
not a design number; the tuned numbers are in the source-layer report).

## 7. Harness findings (relevant beyond this task)

1. **Firefox on an occluded Wayland window serves a stale tree.** Run on the user's compositor, the page flipped to `document.visibilityState ===
   "hidden"` (while `hasFocus()` stayed true) within the first minute, the document lost its `showing` state, DOM mutations continued (JS timers ran)
   but AT-SPI events were delayed by seconds or lost until the next visible moment (many steps: 100 DOM mutation records, 0 events, unchanged
   tree). Gecko processes accessibility updates in a refresh-driver observer (`refs/firefox/accessible/base/NotificationController.cpp:488,689`,
   `WillRefresh`), and an occluded window gets no frame callbacks. Forcing `layout.frame_rate=60` or `docShellIsActive=true` did not help.
   The same applies to a real user: a Firefox window behind others, on another workspace or locked screen will not update its a11y tree.
   gorca should detect `showing` absent on the document / `window:deactivate` and treat the mirror as stale rather than trusting silence.
2. **A private headless gnome-shell works as the Wayland compositor**: `GSETTINGS_BACKEND=memory gnome-shell --headless --wayland --no-x11
   --wayland-display=gorca-wl --virtual-monitor 1280x800` inside the private session bus (see `tests/web/spa/run_characterize.sh`). Firefox then
   registers on the private a11y bus, its window is always visible and the user's focus is untouched. This is also a candidate for end-to-end
   gorca tests against the real Shell a11y tree (the Shell itself is an AT-SPI app on that bus). Failure modes seen: calendar/screencast service
   warnings only.
3. Chromium headless=new needs `Input.dispatchKeyEvent type=keyDown` with `text` for Enter (implicit form submission); `rawKeyDown` alone does
   not generate the `char`.
4. A click can be lost when the focus-leaving blur adds an error message and shifts the button (observed on the form, CDP and probably real):
   the scenario uses Enter instead. Noted because screen reader users also trigger such blur re-layouts.

## 8. Input for the mirror's diff and identity design

1. **Do not trust events alone for order.** Chromium silently reorders (keyed move) and silently flips `showing`/`visible` on scroll; add a cheap
   `GetChildren` revalidation of the *parent* of the cursor (and of the list/table being navigated) on read, plus after every burst that touched
   that parent. A full reconcile is not needed.
2. **Treat `remove` as subtree drop and `add` as subtree fetch; coalesce per parent over a ~30-60 ms window** (first event p95 <= 40 ms, bursts of
   one action end in p50 19-33 ms, p95 ~300 ms for scrolling). Remove events arrive before add events and not in index order; Firefox
   mixes `TextChanged:*/system` between them and sends index -1 / null child in ~13 % / 1.6 % of ChildrenChanged.
3. **Identity ladder**: (1) object path while the object exists; (2) after a remove+add burst under the same parent: match by DOM `id` (validated by
   `role`+`tag`, never trusted when only an attribute changed in Chromium) then by `(role, name, tag)` with sibling order as tie-break - the keyed
   Firefox reverse gives 100 removed/100 added with unique names, the grid sort 658; matching restores cursor/position and avoids re-announcing;
   (3) in-place swaps (unkeyed lists, tab panels) keep the path but change meaning: the diff must key on `(path, name/role signature)`, never on the path
   alone, otherwise "item moved" is reported as "renamed". The research diff (`research/web-graph/diff.py`: keyed LIS + unkeyed matching) already has
   both modes; this data says which signals feed it: 50 `PropertyChange:name` events on 50 distinct paths in one burst = unkeyed rewrite; `remove`x N + `add`x N with
   equal multiset of names = Firefox move.
4. **Name changes are the dominant event** (599 of ~3000 Chromium events; 50 per unkeyed re-render): coalesce per object, keep only the last, and
   fetch the name lazily when the object is next read.
5. **Virtualized feeds**: rows are individual `add`/`remove` events; surviving rows keep their paths; `aria-posinset`/`setsize` are the only
   position cue (kept in the node attributes, not verified here as AT-SPI attributes - see risks). The mirror holds a sliding window; document-order
   indexes (treap) must tolerate 20-80 node churn per scroll step at scroll speed (<= 40 ms bursts).
6. **Live regions**: derive from `ChildrenChanged:add` + `TextChanged:insert` under a node with `container-live`; apply `atomic`/`relevant` from the
   attributes; rate-limit by container (100 messages / 2 s was ~50 events/s).
7. **Focus**: always follow `StateChanged:focused` (Chromium) / `Focus.Focus` (Firefox; also `focused`); for combobox `activedescendant` the event target is the
   option; when the focused node is removed expect a `focused` on the document.

## 9. Implication for the AGP / source-layer decision (B-6)

Everything above was obtained over plain AT-SPI. The measured weak spots are (a) the Chromium silent reorder, which is a Chromium property no
Firefox-side AGP can fix, and (b) Firefox event volume (500 events for 100 renames, 250 per Vue unkeyed prepend), which AGP could reduce at
the cost of a privileged route. Neither blocks the mirror: both are handled by parent revalidation and per-parent coalescing. The SPA data does not
justify AGP; it stays an opt-in accelerator to benchmark on bursts of 500+ events, not a prerequisite.

## 10. Limits and unverified

* One run per scenario per browser (no variance estimate); event counts are repeatable across frameworks (identical steps give identical counts), but
  Chromium's `t_last` has occasional 160-360 ms tails (frame-driven) that single runs cannot characterize.
* Chromium's `showing`/`visible` state flips on scroll or reorder were seen as state differences in snapshots (e.g. 120 nodes after the keyed reverse)
  but which states flip and whether any event is sent was not isolated.
* `aria-posinset`/`aria-setsize` presence as AT-SPI object attributes was not checked (attribute filter in the dumps keeps only the keys listed
  in the real-trees README).
* The wheel scroll in Firefox runs inside the private shell with real wheel events; scroll was verified to have taken place (no synthetic fallback was used).
* Synthetic scroll helpers (`scrollIntoView` before clicks that are outside the viewport) are used by the driver; they may add scroll-related events to the
  first step of a scenario that needs them (virtual feed `End`, infinite scroll use real wheel).
* The hand-rolled windowing in Vue/Angular/Svelte is not a library; React uses react-window. Real libraries (TanStack Virtual, CDK scroll viewport) may recycle DOM nodes
  instead of creating new ones, which would change identity retention (not measured).
* Firefox Window.Activate/Deactivate events came from the private shell and are not representative of a user's desktop.
* The driver's registration (`RegisterEvent object:`) means the browsers emit everything; a real client that registers narrower sets may see fewer events.
