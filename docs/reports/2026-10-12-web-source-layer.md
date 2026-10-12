# Web document source layer: AT-SPI (tuned) vs own protocol (AGP) for Firefox and Chromium

Date: 2026-10-12. Author: web-a11y-engineer. Builds on `2026-10-12-web-a11y-browsers.md`.

Evidence tags: **[live]** measured by a probe in `tests/web/probes/` (private browser instance, own profile),
**[src]** read in `refs/` or upstream source, **[doc]** read in vendor documentation, **[memory]** recalled, unverified.

## 0. Verdict

1. The cost of "pull 100k nodes" is not inherent to AT-SPI. The earlier 1.25-1.6 ms/node figure was a naive sequential client
   (6 sync calls per node). A tuned AT-SPI client (P2P socket, pipelined async calls, one undocumented bulk method, a few
   connections) builds a 100k-element Firefox document in **about 3 s** and a 10k-element one in **about 0.3 s** [live]. That is
   a 50-100x improvement with no browser flags.
2. A privileged in-process source for Firefox is another 4-10x faster for snapshots (229k Gecko nodes with name and state in
   0.75 s, 1.5 s including shipping them to a client) and, more importantly, gives **richer events** than the ATK bridge
   (REORDER, NAME_CHANGE, TEXT_* with the real accessible, no D-Bus per event) [live]. It needs an explicit browser flag
   (`-remote-allow-system-access` + `--marionette`) or an AutoConfig file, and it depends on Gecko-internal XPCOM
   interfaces (stable for a decade, but not a public API).
3. For Chromium the bottleneck is not the transport. The renderer needs about 35 s to serialize a 100k-element page into
   the browser process, regardless of whether the reader is AT-SPI (browser-side tree complete after ~37 s) or CDP
   (`getFullAXTree` 32 s) [live]. CDP is slower than tuned AT-SPI once the tree exists, gives no live-region or caret events,
   re-sends whole nodes, and since Chrome 136 refuses `--remote-debugging-*` on the default profile [doc]. CDP is not worth
   building as a source for a user's Chrome. `chrome.automation` (extension API, desktop platforms) is the only credible
   Chromium AGP candidate and is a later, optional step.
4. Recommended architecture: **SourceLayer abstraction with AT-SPI as the baseline source for both browsers and an AGP-Firefox
   source as an opt-in accelerator**, used in the hybrid mode of section 5 (AT-SPI authoritative for focus, caret and
   interaction; AGP for bulk snapshots, mutation batches, route-change hints, extra events). Build the AT-SPI tuned source
   first, because it is also the fallback that must always work.

## 1. What was measured, and how far to trust it

Host: 16 cores, 30 GB, Debian 13, Firefox ESR 153.4.0, Chrome for Testing (Playwright chromium-1243), at-spi2-core 2.56.2.
Page: `tests/web/pages/gen_large.py N` (N = nominal element count; articles with headings, links, lists, buttons, images;
tables and forms every 200 articles; one live region, one open shadow root). The node count differs per source:

| Source | 1k page | 10k page | 100k page |
|---|---|---|---|
| Chromium CDP AX nodes (incl. StaticText + InlineTextBox, ~3.4x DOM) | 3,347 | 33,867 | 338,677 |
| Chromium AT-SPI nodes (text leaves are nodes) | 2,001 | 21,203 | 212,033 |
| Firefox privileged walk (text leaves are nodes) | 2,260 | 22,878 | 228,782 |
| Firefox AT-SPI nodes (text inline in the parent, no leaf nodes) | - | 10,233 | 102,305 |

Caveats [all]:
- Chrome AT-SPI used `--headless=new --force-renderer-accessibility` on a private a11y bus (no window). Firefox AT-SPI used a
  **headed** private instance with a 300x200 window on the user's Wayland socket, because headless Firefox does not load the
  ATK bridge (it never registered on the bus). One of two headed runs briefly took focus away from the user's window
  (`active_app.py` showed `none` once, `Firefox` the other time); do not repeat without consent.
- Python clients (Gio async calls, multiprocessing). A Rust client will have lower client overhead, so server-side limits
  (below) are the real ceiling. Numbers are single runs on a machine in normal use, not a benchmark; treat +-30 %.
- The Firefox privileged numbers are from a headless instance (same Gecko a11y engine). No window, no real AT-SPI client.
- Not measured: browser main-thread jank caused by the reader (needs a page-side rAF jitter probe; in the bake-off spec).

## 2. Candidate A: AT-SPI tuned to the limit

### 2.1 Techniques and what each gave [live unless noted]

| Technique | Effect |
|---|---|
| Peer-to-peer socket (`Application.GetApplicationBusAddress`) | Both browsers return a usable address (`$XDG_RUNTIME_DIR/at-spi2-XXXXXX/socket`). Skips the bus daemon: GetRole p50 85-136 us vs 163-247 us (earlier report). Gotcha: `atspi_is_initialized() ? "" : addr` in `application-adaptor.c`: a client in a process that called libatspi gets "", so a Rust client must not link libatspi. sun_path limit is 108 chars: a long `XDG_RUNTIME_DIR` makes the address empty. |
| Pipelined async calls (window 16-64 in flight per connection) | 1 sync call at a time: ~150 us/node for `GetChildren` only. Window 64: 24-28 us/node (**6x**). |
| Multiple client processes, own P2P connection each | Server side is the browser's main thread (GLib loop), so scaling saturates: 1 -> 4 connections gave **1.7-2x**, 8 connections no better than 4. Ceiling **~75k calls/s (Chrome), ~95k calls/s (Firefox)**. Beyond ~4 connections gives nothing; do not build a thread-per-node farm. |
| `org.a11y.atspi.Collection.GetTree(rule, properties)` | **Undocumented bulk method** (in `atk-adaptor/adaptors/collection-adaptor.c` `impl_GetTree`, absent from `xml/Collection.xml`; present in Debian's 2.56.2 [src+live]). One call returns the whole subtree as a flat DFS-ordered `a((so)a{sv})`. With properties `[Name, ChildCount, Attributes]` it returns structure + names + `tag`/`id`/`class`/`xml-roles` attributes. **It does not return role, state or interfaces** (those are methods, not properties), so a second pass of `GetRole` + `GetState` (pipelined, paths known, no `GetChildren`) is needed. Passing no properties dumps everything including `Component.ScreenExtents` (2-3x slower). |
| `Collection.GetMatches` for navigation queries | Works (earlier report); my "match all" rule returned 0 results (rule encoding wrong, unresolved). Not needed for mirror building, still the right tool for lazy structural queries. |
| `Cache.GetItems` | Useless for web content (earlier report). |

### 2.2 Build times (time to a mirror with role, state, name, attributes)

| Document | Strategy | Chrome | Firefox |
|---|---|---|---|
| 10k elements | naive sequential, 6 calls/node (earlier report) | est. 34 s for 21k nodes | est. 13 s |
| 10k elements | GetTree(Name,ChildCount,Attributes) alone | 0.21 s | 0.12 s |
| 10k elements | GetTree + pipelined role/state, 4 conns | not run | **0.32 s** |
| 10k elements | full pipelined walk, 4 calls/node (GetChildren, role, state, GetAll), 4 conns | 1.0 s | 0.58 s |
| 100k elements | GetTree(Name,ChildCount,Attributes) alone | 2.5 s | 1.1 s |
| 100k elements | GetTree + pipelined role/state, 4 conns | **6.5 s** | **3.0 s** |
| 100k elements | full pipelined walk, 4 calls/node, 4 conns | 11.2 s | 5.0 s |
| 100k elements | when is the browser-side tree complete? | **~37 s after load** (renderer serialization) | a few seconds (growth poll saw it full at 0.4 s) |

Realistic best case for AT-SPI: **10k nodes 0.3-0.6 s, 100k nodes 3-6 s** after the browser has the tree, with a first
usable result (structure + names, enough for heading navigation and reading the first screen) after GetTree in
**0.1 s / 1 s**. Chrome's extra ~35 s on a 100k page is outside gorca's control (see 3.4).

Design consequences: (a) never block speech on a full build; build lazily around the caret and by role index (headings,
landmarks, links via role/`xml-roles` from GetTree attributes), complete the rest in the background; (b) the mirror must
accept incremental completion ("partial tree" is a first-class state); (c) cap in-flight calls per app to avoid starving
the browser UI thread (the walk itself is browser jank: it runs on the UI thread).

### 2.3 What AT-SPI cannot be tuned out of

Lossy events (REORDER not mapped, caret granularity lost, no live-region signal; see earlier report), the offset unit
differences, and Chromium's tree arriving late. Event volume is a separate problem: one `textContent` change on 100
buttons produced 1,200 Gecko-side events (`HIDE`, `SHOW`, `REORDER`, `TEXT_REMOVED`, `TEXT_INSERTED`, `NAME_CHANGE` x200 each)
[live], each of which becomes an ATK signal and a D-Bus message; the mirror must coalesce by subtree dominance either way.

## 3. Candidate B: Chrome DevTools Protocol (Chromium)

Measured with `--remote-debugging-pipe` (NUL-delimited JSON on fd 3/4; no TCP port) on headless shell [live]:

| Operation | 1k | 10k | 100k |
|---|---|---|---|
| `Accessibility.getFullAXTree` | 0.04-0.1 s, 3.3k nodes, 1.2 MB | 0.67-1.2 s, 33.9k nodes, 12.5 MB | **31-35 s**, 338.7k nodes, **126.7 MB** |
| `queryAXTree` (role=heading) | 0.02 s | 0.26 s | **28.7 s** (rebuilds the tree each call) |
| `DOMSnapshot.captureSnapshot` (DOM + 2 styles) | 0.01 s | 0.08 s | 0.94 s, 22.7 MB |

- Superlinear: 10x nodes -> ~45x time. The cost is renderer-side serialization (matches the 37 s AT-SPI tree arrival).
- Completeness per node: role, `chromeRole`, name **with its source chain** (aria-labelledby, aria-label, label, content, title:
  excellent for debugging, heavy for a mirror), `ignored` + reasons, `childIds`, `parentId`, `backendDOMNodeId`, properties
  (level, focusable, url, invalid, live, atomic, relevant, ...). Description empty in my page; no text offsets, no bounds,
  no per-line data, no selection/caret. `backendDOMNodeId` is a real DOM identity (useful for reconciliation).
- Updates: `Accessibility.nodesUpdated` is sent **only for nodes the client has previously fetched with
  getRootAXNode/getChildAXNodes/getAXNodeAndAncestors/getPartialAXTree** (`nodes_requested_` in
  `inspector_accessibility_agent.cc` [src], verified [live]: renaming 100 buttons that were not fetched produced 0 events;
  appending 50 articles produced 1 event with 1 node (the registered `main`, with new `childIds`)). Events are throttled,
  whole-node (no diff), have no remove/insert op, no live-region announcement, no focus or caret event. `loadComplete` fires
  per document. A route change via `history.pushState` + DOM replacement gave `navigatedWithinDocument` + `loadComplete`
  (page-level hint is good).
- Security: a debugging channel is full control of the browser (cookies, any origin, script execution). Pipe mode avoids a
  listening port (no DNS-rebinding/other-user exposure); the pipe is only reachable by the launching process. But
  **Chrome 136+ ignores `--remote-debugging-port/pipe` when the profile is the default user-data-dir** [doc:
  developer.chrome.com/blog/remote-debugging-port]. gorca cannot attach to a user's normal Chrome; it would have to launch
  Chrome itself with a dedicated profile, which breaks the "use my browser" premise. Chrome for Testing keeps the old
  behavior but is not what users run.
- Verdict: **do not build a CDP source for end-user Chrome.** Keep CDP as a test oracle (it gives the browser's own names and
  roles, which is how `accname_lib_probe.py` validates a DOM-based computation).

### 3.1 `chrome.automation` (extension API) [doc/src, not probed]
`_api_features.json` lists `automation` for `privileged_extension` on `chromeos, linux, mac, win` (manifest permission
`automation`) [src, fetched from chromium.googlesource.com]. It exposes the AX tree and tree-change observers inside the
extension (same serialization cost as above, but incremental thereafter), then needs a bridge to gorca (native messaging or
WebSocket). Branded Chrome 137+ removed `--load-extension`, so installation is by policy (`ExtensionInstallForcelist`) or Web
Store. Not probed; worth a follow-up only if AT-SPI Chromium events prove inadequate.

## 4. Candidate C: Firefox options

### 4.1 Verdict per option

| Option | Gives the a11y tree? | Notes |
|---|---|---|
| WebDriver BiDi | **No tree.** Only `browsingContext.locateNodes` with an `accessibility` locator `{role, name}` (spec section 7.3.2.3) [doc] | A query, not a snapshot; no events. Useful as a test oracle only. |
| Marionette, content context | `WebDriver:GetComputedRole/GetComputedLabel` per element [memory] | One RPC per node; useless for bulk. |
| **Marionette, chrome context** (`-remote-allow-system-access`) | **Yes, full XPCOM** `nsIAccessibilityService` | The flag is mandatory since this Firefox: `Marionette:SetContext chrome` fails with "System access is required. Start Firefox with -remote-allow-system-access" [live]. Used here. |
| Firefox DevTools RDP accessibility actors (`devtools/server/actors/accessibility/walker.js`) | Yes, per node (`children()`, lazy), with events (states-change, name/value/description change, reorder, text-change, index-in-parent) [src, fetched] | Runs in the content process target with `nsIAccessibilityService`; designed for the Accessibility panel, per-node actors, chatty. Needs `--start-debugger-server` + prefs; protocol unstable. Inferior to a purpose-built actor. |
| WebExtension API | **No a11y API** in Firefox | Experiments APIs would give privileged code, but are restricted to Nightly/Dev Edition/unbranded; ESR behavior unconfirmed [doc]. Dead end for stock Firefox. |
| AutoConfig (`mozilla.cfg` + `general.config.filename`) | Yes (chrome JS at startup) | Root-owned file in the install dir, no flag needed, survives restarts; not available in Flatpak/Snap. Supported enterprise mechanism. |
| Regular WebExtension content script computing DOM+ARIA+accname | Reconstruction, not the browser's tree | See 4.3. |

### 4.2 The privileged route in detail (the interesting one) [live]

Bootstrap: launch Firefox with `--marionette -remote-allow-system-access` (loopback TCP, **no authentication**:
`ss` shows `127.0.0.1:28281`), run chrome-context scripts that do three things: (1) `ChromeUtils.registerWindowActor` for a
`JSWindowActorChild` loaded from a `resource://` substitution to a directory we own (system principal in the content
process, worked); (2) have that actor read `nsIAccessibilityService` in-process (`LocalAccessible`, no IPC mirror
involved); (3) open a Unix-domain server socket from chrome JS (`nsIServerSocket.initWithFilename(file, 0600)` created
`srw-------`) and stream snapshots/mutations to gorca over it. Marionette is then only the bootstrap; loopback TCP is
the weak link and should be closed or firewalled after handoff (no API to stop the server short of restart: open question).

| Measurement (headless, same engine) | 10k | 100k |
|---|---|---|
| Parent-process walk (RemoteAccessible), role+name+state per node, JS XPCOM | 22,878 nodes, 58-81 ms | 228,782 nodes, **0.74-0.82 s** (bare role-only walk 0.42 s) |
| Content-process walk (LocalAccessible), role+name+state+compact arrays | 22,878 nodes, 71 ms | 228,782 nodes, 0.63-0.78 s; serialization to 8.5 MB JSON 25 ms |
| Ship compact arrays (1.1 M ints + 195k names) through Marionette JSON/TCP to a Python client | - | **1.49 s total** (walk + transport); a binary Unix-socket channel should cut the transport part |
| Time until the page's tree is complete (page load + mirror) | 2.3 s | 5.3 s (parent mirror), 5.9 s (content) |

Per node ~3 us for several XPCOM calls, versus 50-110 us per node over D-Bus. The unavoidable cost left is Gecko's own
tree construction and, for the parent-process variant, the IPC mirror; the content-process actor avoids the mirror.

Events [live, `accessible-event` observer in the parent via `Services.obs`]:

| Mutation (10k page) | Events seen in the parent |
|---|---|
| rename 100 button labels | 1,200: HIDE, SHOW, REORDER, NAME_CHANGE, TEXT_REMOVED, TEXT_INSERTED x200 each, 19 ms span |
| append 50 articles | 104: SHOW x100, TEXT_INSERTED x2, REORDER x2 |
| 200 live-region updates at 1 ms | 126 (browser coalesced timers): SHOW/TEXT_INSERTED/REORDER x26 |
| remove 20 articles | 44: HIDE x40, TEXT_REMOVED x2, REORDER x2 |
| SPA route (replace `main` subtree + pushState) | 1,786: HIDE x1,752 (one per removed accessible), SHOW x6, REORDER x6, NAME_CHANGE x8, TEXT_VALUE_CHANGE x2 |

Gecko emits hide per removed accessible and `REORDER` per changed parent; the bridge drops REORDER on Linux, the privileged
observer does not. Subtree-dominance coalescing in the source (not the mirror) cuts the 1,752 HIDE events to the few
roots, which is a free 100x reduction on route changes.

Coverage [live, `firefox_frames_probe.py`]: **cross-site iframe content (OOP iframe) is stitched into the parent-process
walk** (depth 2 `ROLE_DOCUMENT`), **closed shadow root content is present**, canvas fallback content and `aria-label` are
present. These are exactly the cases where a DOM-reading extension struggles.

### 4.3 WebExtension content script that recomputes everything

- Library: `dom-accessibility-api` 0.7.1 (14 KB minified bundle). Name agreement against Chromium's own AX names on the
  generated page: **98.9 % identical** on named elements (10k page 4,333/4,381; 100k page 43,350/43,842). Differences are
  implementation-defined spots: `tr` gets a content-derived name from the library but none from Chromium, table `caption`
  is named by the library but Chromium names the table instead. Roles: the library's `getRole` only covers explicit and
  basic implicit roles; full HTML-AAM role mapping, ARIA states with inheritance, hidden/inert computation, `display:contents`,
  CSS-generated content, shadow DOM slotting, `aria-owns` reordering, table/grid structures and text-offset semantics would
  all need to be reimplemented and then kept bug-compatible with each engine [inferred]. This is the "second reference
  implementation" problem; not recommended as the primary tree.
- Cost in page: 130 ms for 10k elements, **16.2 s for 100k elements** (Chrome, library run on the main thread: superlinear,
  likely style recalculation), i.e. it competes with the page's JS thread.
- Reach limits [memory/doc]: closed shadow roots need `Element.openOrClosedShadowRoot` (Firefox) and only work in the content
  script's own frame; cross-origin iframes need `all_frames` injection and message plumbing; no access to canvas pixels or
  to the accessibility of `<canvas>` custom drawing beyond fallback DOM; PDF.js viewer in Firefox is a privileged page
  extensions cannot script (so PDF would still need AT-SPI).
- Transport: native messaging limits are asymmetric: **browser -> host 4 GB, host -> browser 1 MB** [doc: MDN], 32-bit length
  prefix JSON. Snapshots fit; gorca -> page commands must stay small. Alternative: WebSocket to a loopback listener (no
  host manifest, but loopback exposure). Extension signing: AMO (unlisted) or policy `ExtensionSettings`.
- Verdict: **valuable only for what AT-SPI and the privileged route cannot give** (DOM ids/class/data-*, route-change and
  mutation-batch hints, visited state, `scrollIntoView`/focus, reading-order hints), not as a tree source.

## 5. Candidate D: hybrid, and reconciling identities

Roles of each side:

| Concern | Owner |
|---|---|
| Focus, caret, selection, text-at-offset, editing, actions, bounds | AT-SPI (authoritative; it is what applications and the compositor agree on) |
| Bulk snapshots, rebuilds after route change, resync | AGP when present, else tuned AT-SPI (GetTree) |
| Mutation batches with batch boundaries | AGP (per-batch ops with sequence numbers), else AT-SPI event coalescing |
| REORDER, name/description change on the node, live-region announcements with politeness | AGP (full fidelity), else AT-SPI container-live attributes |
| Page-level hints (route change, document ready, title) | AGP |

Node identity. There is no shared stable id:
- AT-SPI object paths are bridge-assigned counters (`/org/a11y/atspi/accessible/N`); neither browser implements
  `AccessibleId` (`atk_object_get_accessible_id`, grep of `refs/firefox/accessible/atk` and `refs/chromium/ui/accessibility/platform` found nothing) [src].
- Gecko `nsIAccessible.uniqueID` (int64) and `.id` (DOM id), Chromium `backendDOMNodeId` identify nodes on the AGP side.
- Both AT-SPI servers expose the `id`, `class` and `tag` object attributes (Firefox `LocalAccessible.cpp:1248-1258`, needs the
  DOMNodeIDAndClass cache domain; Chromium `ax_platform_node_base.cc:1440-1449`), but most nodes have no DOM id.

Proposal: **lockstep alignment by position**. `GetTree` returns a DFS-ordered flat list with `ChildCount`; an AGP snapshot is
a DFS-ordered flat list with child counts. Both describe the same accessibility tree (the AGP walks the very tree that the
ATK bridge exposes: Gecko `nsIAccessible` parent/child order is the order ATK exposes), so zipping the two lists gives a
bijection, verified by `(role, name, tag/id when present)` agreement. A mismatch marks the local subtree "unreconciled" and
falls back to AT-SPI-only for that subtree; the mapping `agp_id <-> atspi_path` is stored per node. Differences to normalize
before zipping: Gecko text leaves exist only on the AGP side (102k vs 229k nodes), so the AGP serializer should fold text
leaves into the parent's text (what ATK does), or the mirror should own the folding rule in one place. Because AT-SPI
events arrive keyed by path and AGP events keyed by AGP id, the mirror keeps both keys per node and treats the AGP batch as
truth for structure and the AT-SPI event as truth for focus/caret. Sequence numbers on AGP batches give gap detection: gap
or unreconciled subtree triggers a targeted re-snapshot (one `GetTree` on the subtree root, or an AGP subtree dump).

## 6. Long-term support risk

| Option | API stability | Vendor policy trend | Packaging (Flatpak/Snap/distro) | Overall risk |
|---|---|---|---|---|
| AT-SPI standard methods | Stable (D-Bus API) | Both vendors invest in it (Orca/GNOME); Firefox Linux a11y owned by Mozilla | Works in Flatpak/Snap with the a11y bus socket exposed (Flatpak exposes `org.a11y.Bus`; P2P sockets under `$XDG_RUNTIME_DIR` may not be reachable from a different sandbox: **unverified**, fallback is via the bus daemon) | Low |
| `Collection.GetTree` | Undocumented; present in 2.56.2; could change signature | Not in the XML spec | Same as above | Medium; guard with feature detection + fallback to per-node walk |
| Firefox privileged (Marionette / AutoConfig + actor) | XPCOM `nsIAccessible*` stable for years but internal; `JSWindowActor`/`resource://` internal | Mozilla keeps tightening privileged access (note the new `-remote-allow-system-access` gate); AutoConfig is an enterprise feature that persists but the sandbox pref has been tightened before | Flatpak/Snap: cannot write the install dir (AutoConfig out), flags need a wrapper launcher; Marionette flags can be set in `.desktop` override | Medium-high; needs a version-compat test matrix and a clean fallback to AT-SPI |
| CDP | Very stable protocol | Chrome 136 restricted default-profile use to curb infostealers; trend is more restriction | Fine for own-launched Chrome only | High for end-user Chrome |
| `chrome.automation` | Stable since 2016, used by ChromeVox | Extension manifest V3 and branded-Chrome sideload restrictions | Policy-install only | Medium |
| Firefox WebExtension (DOM-based) | `webextensions` API stable | Mozilla signing requirement | Works everywhere (policy-install) | Low, but permanent quality tax |
| BiDi | Standardized, evolving | W3C | Fine | Not a tree source |

Browser-detection note: a screen reader that is not named `orca` is not detected by Chromium (earlier report); that affects
the AT-SPI Chrome path the same way, independent of this decision.

## 7. SourceLayer interface sketch

Design goals: the mirror (diff, indexes, flat text, live policy) is source-agnostic; sources declare capabilities; every call
carries a deadline; partial trees are a normal state; sources never block the mirror actor (each source is its own actor
and talks over channels).

```rust
/// What the mirror may assume. Checked, not guessed (feature detection at attach time).
pub struct Capabilities {
    pub bulk_snapshot: bool,            // can return a whole subtree in O(1) round trips
    pub batch_boundaries: bool,         // updates arrive as sequenced, atomic batches
    pub reorder_events: bool,
    pub live_announcements: bool,       // politeness + text, not just "container-live" attributes
    pub text_leaves_as_nodes: bool,     // Gecko-AGP true, Gecko-ATSPI false, Chromium both true
    pub offset_unit: OffsetUnit,        // Utf16WithFeffAfterAstral | CodePoint | Utf8Bytes
    pub stable_ids: IdStability,        // PerSession | PerDocument | Volatile
    pub carries_bounds: bool,
    pub carries_dom_hints: bool,        // tag/id/class/data-*, route-change hints
    pub trust: Trust,                   // Authoritative | Advisory (what wins on conflict)
}

pub struct DocKey { pub app: AppId, pub doc: SourceDocId }   // opaque per source
pub struct NodeKey(pub u64);                                 // source-local, never reused within a DocKey epoch

pub struct SnapshotRequest {
    pub root: Option<NodeKey>,          // subtree resync; None = whole document
    pub depth_first_budget: Budget,     // max nodes, max bytes, max wall time; source returns a partial tree + cursor
    pub fields: FieldSet,               // STRUCTURE | NAME | ROLE | STATE | ATTRS | TEXT | BOUNDS (cheap-first order)
}

pub enum SnapshotChunk {
    Nodes { epoch: Epoch, nodes: Vec<NodeRecord> },   // flat DFS order, child_count per node
    Complete { epoch: Epoch },
    Cursor(ResumeToken),                             // budget exhausted; call again to continue
}

pub struct NodeRecord {
    pub key: NodeKey, pub parent: Option<NodeKey>, pub child_count: u32,
    pub role: Option<RawRole>, pub states: Option<StateBits>, pub name: Option<Arc<str>>,
    pub description: Option<Arc<str>>, pub attrs: Option<AttrMap>, pub text: Option<TextRun>,
    pub extra_id: Option<ExtraId>,       // second identity (AT-SPI path | uniqueID | backendDOMNodeId)
}

pub enum SourceEvent {
    Batch { doc: DocKey, seq: u64, ops: Vec<Op> },   // Op = Insert{parent,index,records} | Remove{key} | Update{key,fields} | Reorder{parent,order}
    Focus  { doc: DocKey, node: NodeKey },
    Caret  { doc: DocKey, node: NodeKey, offset: u32, kind: CaretCause },
    Announce { doc: DocKey, node: Option<NodeKey>, politeness: Politeness, text: Arc<str> },
    DocLoaded { doc: DocKey }, RouteChanged { doc: DocKey, url: Arc<str> }, DocGone { doc: DocKey },
    Gap { doc: DocKey, reason: GapReason },          // sequence break, overflow, source restart: mirror must resync
    SourceLost { reason: String },
}

#[async_trait]
pub trait DocumentSource: Send {
    fn caps(&self) -> &Capabilities;
    /// Every call: explicit deadline; Err(Timeout) must leave the source usable.
    async fn documents(&mut self, dl: Deadline) -> Result<Vec<DocKey>, SourceError>;
    async fn snapshot(&mut self, doc: &DocKey, req: SnapshotRequest, dl: Deadline) -> Result<SnapshotStream, SourceError>;
    async fn text(&mut self, doc: &DocKey, node: NodeKey, range: Range<u32>, dl: Deadline) -> Result<TextSlice, SourceError>;
    fn events(&mut self) -> mpsc::Receiver<SourceEvent>;        // bounded; overflow becomes Gap, never blocks the source
    // Actions that must go through the authoritative path:
    async fn set_caret(&mut self, doc: &DocKey, node: NodeKey, off: u32, dl: Deadline) -> Result<(), SourceError>;
    async fn focus(&mut self, doc: &DocKey, node: NodeKey, dl: Deadline) -> Result<(), SourceError>;
}

/// Composition: bulk/batches from `bulk`, focus/caret/text/actions from `authority`.
pub struct HybridSource { authority: Box<dyn DocumentSource>, bulk: Box<dyn DocumentSource>, map: IdMap /* NodeKey<->NodeKey, section 5 */ }
```

Notes: (1) The mirror sees one `NodeKey` space; `HybridSource` owns the id map and emits unified keys. (2) `Capabilities.trust`
decides conflict resolution (AT-SPI focus beats AGP focus; AGP structure beats AT-SPI structure when sequenced). (3) Source
selection is per app and per document, with automatic downgrade on `SourceLost`/`Gap` storms (state machine as an `enum`:
`Detached -> Attaching -> Snapshotting{progress} -> Live -> Resyncing -> Degraded(to AT-SPI)`). (4) Subtree-dominance and
dedupe live in the source adapter (cheap, near the data) and again in the mirror (cross-source).

## 8. Bake-off spec (use the `/bakeoff` skill; this is a key decision)

Question decided: **which source is the default for Firefox web content, and is AGP worth its maintenance cost?**

Candidates (each implements `DocumentSource`, same Rust crate skeleton, own worktree):
1. **ATSPI-TUNED**: P2P, `GetTree(Name,ChildCount,Attributes)` + pipelined role/state, 4 connections, windowed in-flight,
   lazy completion around the caret, event coalescing; no flags.
2. **AGP-FF**: Marionette bootstrap + `JSWindowActor` in the content process + Unix socket (binary framing, e.g. a compact
   columnar snapshot and sequenced batches), `accessible-event` with subtree dominance in JS; opt-in flag.
3. (optional) **AGP-FF-WEBEXT**: WebExtension content script + WebSocket, DOM-based, only as the cost reference for "no privileged
   flags" and for the hint channel.

Pages (all under `tests/web/pages/`; existing: `gen_large.py`, `frames/`; to build: marked *new*):
1k/10k/100k from `gen_large.py`; `frames/` (cross-site iframe, closed shadow, canvas); *new* infinite scroll (append 50 items
per 200 ms); *new* SPA route change (full `main` replacement, 1k and 20k nodes); *new* aria-live flood (1k updates/s for 10 s,
polite and assertive); *new* 10k-row virtualized grid (aria-rowindex, scroll-driven mutation); *new* large `contenteditable`
(100 KB text); *new* PDF.js sample; *new* deep nesting (depth 500) and wide (50k siblings).

Metrics (report p50/p99 over 20 runs, cold and warm; machine with Orca off, RT kernel as in CLAUDE.md):
1. **Time to first navigable**: attach -> first heading/landmark index usable; and **time to full mirror** at 1k/10k/100k.
2. **Mutation latency**: DOM mutation -> mirror updated, for 1 / 100 / 1,000 changed nodes, and route change.
3. **Event storm**: sustained events/s accepted without gap at 1k, 10k, 50k/s; gap rate; memory growth over 5 minutes.
4. **Browser impact**: page-side rAF jitter probe and long-task count during snapshot and during storms (target: no frame >50 ms
   attributable to the reader); browser main-thread CPU time (from `/proc` per-thread) per snapshot.
5. **gorca cost**: CPU time and peak RSS of the source, bytes on the wire.
6. **Fidelity**: mirror vs oracle (BiDi `locateNodes` / Marionette `GetComputedLabel` for sampled nodes + AT-SPI GetTree) -
   role/name/state agreement %, missing/extra nodes, live-region announcements delivered vs expected (politeness correct).
7. **Robustness**: kill -STOP the browser for 5 s mid-snapshot; browser crash and restart; extension/actor reload; 100 tab
   open/close; must degrade to `Degraded` without wedging other apps; resync time.
8. **Coverage matrix**: cross-site iframe, closed shadow, canvas, PDF.js, `aria-owns`, `display:contents`, contenteditable carets.
9. **Operational cost**: lines of code, number of Gecko-internal interfaces touched, flags/files needed, and a Firefox
   ESR-N / ESR-N+1 / release compatibility run.

Decision rule suggestion: AGP-FF becomes default only if it wins metrics 1-3 by >=5x **and** survives 7-9; otherwise ATSPI-TUNED
stays default and AGP-FF ships as opt-in. Judge: reviewer on measured numbers only.

## 9. Open questions and unverified items

- Rust client numbers (this report used Python): expected better, ceiling set by the browser, not measured.
- `Collection.GetTree` on older at-spi2-core (Ubuntu LTS, Flatpak runtimes): version it appeared in unknown; need detection.
- P2P socket reachability across Flatpak/Snap boundaries: unverified; Firefox Snap's AT-SPI path is a known pain point.
- Marionette loopback TCP has no authentication; handoff to a Unix socket then closing the Marionette listener was not tested.
- AutoConfig as flag-free bootstrap: not tested (needs root writes to `/usr/lib/firefox-esr`).
- Browser-side behavior when the privileged actor's `nsIAccessibilityService` forces the a11y engine on in the content process
  (page perf cost with a11y on) not measured; it is also paid by AT-SPI.
- First actor walk after navigation once returned 1 node (document accessible not populated yet): needs a `document-ready`
  signal instead of polling.
- `chrome.automation` not probed. GetMatches "match all" rule encoding unresolved. Chrome AT-SPI measured headless only.
- DOM-based role computation completeness not quantified beyond names.

## 10. Files

- Probes: `tests/web/probes/` (README table lists all; new ones: `cdp_probe.py`, `accname_lib_probe.py`, `marionette_client.py`,
  `firefox_a11y_probe.py`, `firefox_content_actor_probe.py`, `ff_actor/GorcaProbeChild.sys.mjs`, `firefox_events_probe.py`,
  `firefox_frames_probe.py`, `atspi_tuned_walk.py`, `private_a11y_env.sh`, `run_chrome_atspi.sh`, `run_firefox_atspi.sh`, `active_app.py`).
- Pages: `tests/web/pages/gen_large.py`, `tests/web/pages/frames/`.
- Raw outputs were written to the session scratchpad (not kept); rerun the probes for fresh numbers.
