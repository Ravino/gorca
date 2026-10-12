# Web accessibility in Firefox and Chromium on Linux, and requirements for gorca's virtual document model

Date: 2026-10-12. Author: web-a11y-engineer.

Evidence levels used below:
- **[src]** read in `refs/` source (path cited).
- **[live]** measured on this host against the running browsers, read-only (probes in `tests/web/probes/`).
- **[inferred]** reasoned from source or numbers, not directly measured.
- **[memory]** recalled, not verified in this session. Treat as a lead to confirm, not a fact.

Host under test: Firefox ESR 153.4.0 (toolkit "Gecko"), Chrome for Testing (toolkit "Chromium"), Orca running, at-spi2 on `unix:path=/run/user/1000/at-spi/bus`. Page content was never printed; only roles, counts and timings.

---

## 1. Firefox on Linux

### 1.1 Architecture of the AT-SPI path [src]

- Firefox does not speak AT-SPI directly. It implements **ATK** (`refs/firefox/accessible/atk/`), and loads `libatk-bridge-2.0` (the at-spi2-core `atk-adaptor`) in the **parent process** (`atk/Platform.cpp`: `PlatformInit`, `LoadGtkModule(sAtkBridge)`, `sAtkBridge.init`). The atk-adaptor then turns ATK vtable calls into D-Bus replies on the app's own D-Bus socket.
- Web content lives in **content processes**. Each content process keeps a `LocalAccessible` tree. The parent process keeps a mirror, `RemoteAccessible` (`ipc/RemoteAccessible.cpp`, `ipc/DocAccessibleParent.cpp`), kept current by IPDL messages (`RecvShowEvent`/mutation batches, `RecvCache`, `RecvEvent`, `RecvCaretMoveEvent`, `RecvFocusEvent`, `RecvAnnouncementEvent`, ...). `docs/Architecture.md`: "An accessibility client communicates only with the parent process", and out-of-process iframes are stitched in so the client sees one tree.
- Consequence: **AT-SPI queries on web content are answered from the parent-process cache without a content-process round trip**, provided the needed *cache domain* is active (1.5). The D-Bus server runs on the parent process main thread (GLib main loop), so a busy Firefox UI delays every reply. [inferred]
- Enablement: `atk/Platform.cpp::ShouldA11yBeEnabled` honors `GNOME_ACCESSIBILITY`-style env, then `org.a11y.Status.IsEnabled` via a D-Bus proxy (re-checked on `PropertiesChanged`), then GSettings `toolkit-accessibility`. gorca **must set `IsEnabled=true` on `org.a11y.Bus`** (and keep it true) or Firefox will not build its a11y tree. `GetCacheDomainsForKnownClients` is a no-op on Linux (`atk/Platform.cpp:263`), so there is no screen-reader-specific mode like on Windows.

### 1.2 Tree shape and roles [src + live]

- **Text leaves are not exposed.** `AccessibleWrap::GetNativeInterface` returns no ATK object for `IsText()` accessibles; `getChildCountCB`/`refChildCB` use `EmbeddedChildCount()/EmbeddedChildAt()`; `getIndexInParentCB` comments "we don't include text leaf nodes as children in ATK" (`atk/AccessibleWrap.cpp:272-320, 692-745`). Text lives in the parent's `Text` interface. [live] The invariant "number of U+FFFC characters in a node's text == its child count" held for 108 of 108 text-bearing nodes (`hypertext_invariant.py`).
- A paragraph and a generic `div` are both commonly `ATK_ROLE_SECTION`/`PARAGRAPH` depending on tag; live survey of a 156-node page showed `section` 102, `image`, `button`, `link`, `canvas`, one `document web`. Role mapping is in `base/RoleMap.inc` (`DOCUMENT -> ATK_ROLE_DOCUMENT_WEB`, `NON_NATIVE_DOCUMENT -> DOCUMENT_FRAME`, `HEADING`, `LANDMARK`, `FORM`, `PARAGRAPH`, `SECTION`).
- ARIA/HTML detail is in **object attributes**, not roles: `tag` (element name, `LocalAccessible.cpp:1258`), `xml-roles` (ARIA role string, or the landmark name for native landmarks, `LocalAccessible.cpp:1136-1146`), `level`, `live`/`container-live`/`atomic`/`container-atomic`/`relevant`/`busy`, `id`, `class`, `display`, `explicit-name`, `details-from` etc. gorca needs these attributes for headings, landmarks and live regions.
- Interface set per node [src `atk/AccessibleWrap.cpp:CreateMaiInterfaces`, live]: every node has `Accessible`, `Component`, `Action` (always, "because aria-actions can define actions mid-life"); text-capable nodes have `Text`+`Hypertext`+`EditableText` (note: `EditableText` is advertised even on read-only text, 328/374 nodes in one live sample); links add `Hyperlink`; images `Image`; tables `Table`/`TableCell`; the document adds `Document`. **`Collection` is advertised on every node** [live] because it is provided by the atk-adaptor, not by Firefox.
- Background tabs: all tab documents sit in the tree; only the foreground one has `STATE_SHOWING` [live: 8 `document web` nodes, 1 showing]. A client walking the app must filter by `SHOWING` and must not eagerly descend into every tab.
- Document-level attributes: `Document.GetAttributeValue("DocURL")` and `"MimeType"` (`atk/nsMaiInterfaceDocument.cpp`). No `get_text_selections` (ATK 2.52 document selection API): Firefox does not implement it ("We don't support get_document or set_attribute right now").

### 1.3 Text and hypertext [src]

- `Text` is on the **hypertext container**; child elements that are links/inline elements/embedded objects appear as `U+FFFC` in the parent's text, one per exposed child (verified invariant above). `Hypertext.GetLinkIndex(charOffset)`, `GetNLinks`, `GetLink(i)` map offsets to children (`atk/nsMaiInterfaceHypertext.cpp`). `Hyperlink` gives start/end index and URI (`atk/nsMaiHyperlink.cpp`).
- **Offsets are UTF-16 code units, not code points.** To make ATK (which expects code points) work, `DOMtoATK.cpp` inserts a **U+FEFF (BOM) after every non-BMP character** so offsets line up (`atk/DOMtoATK.h:10-28`, `AddBOMs`). gorca must: (a) treat U+FEFF immediately after an astral character as a phantom placeholder when counting grapheme/code points, (b) never assume `offset == char index` for Firefox text.
- Boundary queries use the old `AtkTextBoundary` entry points (`getTextAtOffsetCB`, `...BeforeOffset`, `...AfterOffset`; no `get_string_at_offset` granularity callback), executed in the parent process on cached text (`CacheDomain::Text`, `TextBounds` for line starts). `GetCharacterExtents`/`GetRangeExtents` need `TextBounds`+`Bounds` domains.
- `text-insert`/`text-remove` events carry the inserted/removed string (`MaiAtkObject::FireTextChangeEvent`, `AccessibleWrap.cpp:1059-1100`), with a `::system` detail when not from user input.

### 1.4 Events [src]

All mapping is in `atk/AccessibleWrap.cpp` (`PlatformEvent` and friends, lines ~880-1170).

| Gecko event | AT-SPI signal | Notes |
|---|---|---|
| DOCUMENT_LOAD_COMPLETE | `document:load-complete` | only when target `IsDoc()`; also emits `activate` for ARIA dialogs |
| DOCUMENT_RELOAD / LOAD_STOPPED | `document:reload` / `document:load-stopped` | |
| SHOW / HIDE | `children-changed::add` / `::remove` (with child object and index) | `FireAtkShowHideEvent`; if the parent is already gone on hide the event is **dropped** (comment at line 1127) |
| REORDER | none | not mapped on Linux; clients only see add/remove |
| TEXT_INSERTED/REMOVED | `text-changed:insert/delete` (new `text-insert`/`text-remove` with string) | |
| CARET_MOVE | `text-caret-moved(offset)` | **granularity, collapsed flag and from-user are discarded** (`PlatformCaretMoveEvent`, line 1021). Fired on the focused hypertext, not on the document |
| TEXT_SELECTION_CHANGED | `text-selection-changed` | |
| FOCUS | `focus` + `state-changed:focused` | suppressed if the root is not yet "activated" (line 1009) |
| STATE_CHANGE | `state-changed:*` | via `gAtkStateMap` |
| NAME_CHANGE | `property-change:accessible-name` | |
| VALUE_CHANGE | `property-change:accessible-value` only when numeric | text-field value changes arrive as text events instead |
| ANNOUNCEMENT (`ariaNotify`, `aria-live` announce API) | `notification(string, ATK_LIVE_POLITE/ASSERTIVE)` | needs ATK >= 2.50 |
| SELECTION_WITHIN, SELECTION_* | `selection-changed` | |
| WINDOW_* | `window:activate/deactivate/...` | |

- **Live regions have no dedicated signal.** A client sees `text-insert`/`children-changed::add` on a node whose object attributes (`container-live`, `container-atomic`, `container-relevant`, `container-busy`) say it is in a live region. Firefox sets `live` from the ARIA role defaults when `aria-live` is absent (`LocalAccessible.cpp:1167-1176`; `nsAccUtils::SetLiveContainerAttributes`). gorca must fetch those attributes to classify an event, and the attribute fetch must be cacheable (live regions flood).
- **SPA route changes produce no document-level event.** Only mutation events (add/remove) plus possibly a title/name change. [inferred from the table: only real navigations create a new `DocAccessible` and a new `load-complete`.] gorca needs a heuristic (subtree replacement near the document root, `DocURL` change, title change). This needs live confirmation with the test pages (section 6).

### 1.5 Cache domains [src]

`base/CacheConstants.h` defines the per-document cache domains pushed from content to parent: `NameAndDescription, Value, Bounds, Resolution, Text, DOMNodeIDAndClass, State, GroupInfo, Actions, Style, TransformMatrix, ScrollPosition, Table, TextOffsetAttributes, Viewport, ARIA, Relations, TextBounds, APZ` (`All` = everything).

- **Default on Linux: only `NameAndDescription | State`** (`nsAccessibilityService.h:123`). Everything else is activated **lazily, on first use**: `RemoteAccessible` getters call `mDoc->RequestDomainsIfInactive(...)`; if a needed domain is missing, the call returns an empty/default value **and** asks all content processes to start caching that domain (`DocAccessibleParent.cpp:87-117`, `nsAccessibilityService::SetCacheDomains`), which re-queues cache updates for **every accessible in every document** (`nsAccessibilityService.cpp:2081+`).
- Implications for gorca:
  1. The **first** query of a new kind (first `GetText`, first `GetRangeExtents`, first table query, first `GetAttributes`) may return empty/wrong data, followed by a burst of cache updates over IPC. Orca tolerates this because it re-queries; gorca must treat "empty text on a node that has children" as "cache cold, retry after the cache-update event", or **warm all domains once at startup** with a throwaway query set.
  2. The warm-up costs a full-tree cache push per document; do it before the user's first navigation, off the critical path.
  3. `CacheDomainActivationBlocker` is used for name-change events so "a passive event doesn't activate a domain" (`AccessibleWrap.cpp:964`). Event handlers in gorca should avoid being the first requester of a domain on the hot path.
- The cache is **per document**; each document escalates independently only for print docs, otherwise it is process-global.

### 1.6 Cost model: how many D-Bus round trips to "read the whole page"

Measured on this host (RT kernel, dbus-broker session, Python/GDBus client, so client overhead is included; treat as upper-ish bounds). [live]

| Operation (Firefox, showing document) | p50 | p99 |
|---|---|---|
| `Accessible.GetRole` via bus daemon | 110-180 us | ~1.0 ms |
| `Accessible.GetChildren` | 162 us | 1.2 ms |
| `GetState` / `GetAttributes` / `GetInterfaces` | 120-195 us | ~1.1 ms |
| `Text.GetText(0,-1)` (190 chars) | 207 us | 1.4 ms |
| `Text.GetStringAtOffset(line)` | 180 us | 1.3 ms |
| `Text.GetRangeExtents(0,1)` | 317 us | 1.6 ms |
| `GetRole` over **peer-to-peer socket** (`Application.GetApplicationBusAddress`) | 85-136 us | 0.7-1.0 ms |
| `Collection.GetMatches` (1 role, whole 156-node document) | ~0.3 ms | ~1.2 ms |

- **Naive full-tree mirror** (`bulk_walk.py`: 1 `GetChildren` per node + `GetRole`, `GetState`, `GetInterfaces`, `Name`, `GetAttributes` = 6 calls/node): 156 nodes, 936 calls, 194 ms = **1.25 ms/node**. Chrome: 353 nodes, 2118 calls, 577 ms = 1.6 ms/node. Extrapolation (sequential): 10k nodes ~ 12-16 s; 100k nodes ~ 2-3 min. Adding text (`GetText`, hypertext, extents) roughly doubles it. [inferred extrapolation]
- **`org.a11y.atspi.Cache.GetItems` does not help for web content.** For Firefox it returned 168 items (53 KB, 3.4 ms) containing only browser chrome (menus, tabs, buttons, scroll panes), no document nodes; Chrome returned 4 items (3 frames + app). [live] The bulk-cache path of at-spi2 (`atk-adaptor/accessible-cache.c`, eager breadth-first `add_pending_items` on the app main thread) therefore cannot be relied on to deliver the page. A client must walk the document itself. [inferred: the web documents are RemoteAccessible wrappers that the eager traversal does not reach; confirm by comparing GetItems size before and after loading a heavy page]
- **Why Orca is slow on large pages** [src `refs/orca/src/orca/`, inferred for the counts]:
  1. Orca has **no mirror**. `scripts/web/script_utilities.py::_get_line_contents_at_offset` rebuilds each line on demand from geometry: `_get_extents` (`GetRangeExtents`) for the anchor, then walks `find_previous_context`/`find_next_context` neighbours, for each one `_get_contents_for_obj` (text-at-offset + `U+FFFC` child lookup), another `_get_extents`, `get_nearest_block_ancestor` / `get_nearest_table_row` (parent chains), `get_label_covering_object` (relations), `get_document_for_object` (parent chain to the document) per candidate. A typical line costs tens of round trips, plus ancestor walks that repeat on every line. At ~0.15-0.3 ms per call that is 5-20 ms per line; reading a page by lines is `lines x that`.
  2. Structural navigation (`structural_navigator.py::_get_all_headings`, etc.) runs `Collection.GetMatches` over the **whole document** for every keypress and then locates the current object by index in the returned list (`_get_object_in_direction`). One round trip but a full server-side tree walk in Firefox's main thread each time (0.3 ms for 156 nodes here; linear in size) and the reply marshals every matching reference.
  3. Caches are time based: `ax_cache_manager.py` clears every 60 s and on events; events invalidate broadly, so a chatty page keeps the caches cold.
  4. Event handling on a page with live regions does synchronous D-Bus queries (`GetAttributes` for `container-live`, text fetches) per event in the Python main loop.
- **libatspi uses peer-to-peer connections to apps**: the 41 messages visible on the bus when a libatspi client walked a 156-node page were only discovery (`GetApplicationBusAddress`, `GetChildAtIndex` on the registry, `GetItems`); the per-node calls are not visible on the bus daemon. [live, `dbus-monitor` filtered to the client's unique name; consistent with P2P, not proven by a socket trace] A Rust client should do the same: connect to `Application.GetApplicationBusAddress` and talk directly. Measured gain: p50 -35 to -60%, p99 -15 to -30% on this host.

### 1.7 Known bugs and workarounds

Verified (cited from Orca's code, which must work around them) [src]:
- Mozilla bug **1879750**: bogus `SECTION` child of a `FRAME` in Gecko windows; Orca ignores it (`refs/orca/src/orca/ax_object.py:176-195`).
- Mozilla bug **1882794**: Firefox dialogs/alerts belong to an app unknown to AT-SPI2 and cannot be the active window normally (`ax_utilities.py:262-273`).
- Mozilla bug **1862437**: `TableCell.GetRowSpan`/`GetColumnSpan` must be fetched individually, not via `GetRowColumnSpan` (`ax_table.py:714-720`).
- Mozilla bug **1867044**: stale cached state after a browser-UI page switch (`selected` on tab); Orca drops its caches on that event (`scripts/web/script.py:1460-1464`).
- Orca also has a deprecated filter for "browser UI autocomplete noise" and "browser UI page switch" events (`event_is_browser_ui_autocomplete_noise_deprecated`, `event_is_browser_ui_page_switch`), meaning Firefox UI events leak into the web script's stream.

From source reading (design hazards rather than filed bugs) [src/inferred]:
- Cold cache domains return empty data on first query (1.5).
- Hide events with no remaining parent are dropped (`AccessibleWrap.cpp:1127`), so `children-changed::remove` cannot be trusted as the only deletion signal; gorca must also tolerate `defunct`/`InvalidArgs` replies for IDs that vanished.
- Caret events lose granularity and user-origin (`PlatformCaretMoveEvent`), so "was this caused by my key" has to be tracked by gorca.
- UTF-16/BOM offsets (1.3).
- `Action`/`EditableText` advertised broadly: never use interface presence as a role signal.

[memory, unverified] Historically reported Firefox/Orca problems: slow `GetText` boundary queries on very long lines; focus/caret events racing document load; `text-changed` floods on live updates. Not confirmed in this session; to be reproduced with the stress pages.

---

## 2. Chromium on Linux (differences from Firefox)

### 2.1 Architecture [src]

- `ui/accessibility/platform/ax_platform_node_auralinux.cc` (5.4k lines) implements **ATK** in the **browser process** for both native UI (Views) and web content. Web content is `BrowserAccessibilityAuraLinux` (`browser_accessibility_auralinux.*`) wrapping `AXPlatformNodeAuraLinux`. Like Firefox, `atk-bridge` (`atk_util_auralinux_gtk.cc`: `atk_bridge_adaptor_init`) turns it into AT-SPI. So **the same atk-adaptor layer, the same Collection, Cache, P2P behavior** apply.
- The renderer serializes its tree as **incremental `AXTreeUpdate`s** (`ui/accessibility/ax_tree_update.h`, `ax_tree_serializer.cc`); the browser process applies them to an `AXTree` (`ax_tree.cc`) and an `AXEventGenerator` (`ax_event_generator.cc`) **derives events by diffing**, then `BrowserAccessibilityManagerAuraLinux::FireGeneratedEvent` maps them to ATK. This is the same shape as the model gorca wants (mirror + incremental updates + derived events), and a design reference for gorca's own diffing.
- Enablement: `AtkUtilAuraLinux::ShouldEnableAccessibility` checks env `ACCESSIBILITY_ENABLED`/`GNOME_ACCESSIBILITY`/`QT_ACCESSIBILITY`, then `org.a11y.Status IsEnabled`, then GSettings. **Screen reader detection is by scanning `/proc` for a process whose cmdline argv has an element exactly `orca` or whose `comm` is `orca`** (`content/browser/accessibility/browser_accessibility_state_impl_auralinux.cc`: `IsOrcaProcess`, `DiscoverOrca`). When found, Chromium creates a process-wide `ScopedAccessibilityMode(kAXModeComplete | kScreenReader | kFromPlatform)`. **gorca will not be detected**, so Chromium may run in a lower AX mode (no `kScreenReader`, possibly fewer extended properties/inline text boxes), affecting line boundaries, live regions and image labels. Options in section 5; needs an empirical test.
- Progressive accessibility (`content/browser/accessibility/README.md`): web a11y is turned on per `WebContents` when an AX mode flag is requested; the first time a page is made accessible the renderer sends a **full tree** (a burst) and then incrementals.

### 2.2 Tree shape and interfaces [src + live]

- Unlike Firefox, **static text nodes are real children**: live sample of a 353-node page: `section` 169, `static` 80, `link` 34, `image` 33, `landmark` 13, `button` 9, `paragraph` 6, `list item` 3, `list` 2, `heading` 1, `notification` 1. Parent text still contains `U+FFFC` for embedded objects, but the 1:1 invariant **does not** hold: 72 of 300 text nodes had `U+FFFC` count != child count (`hypertext_invariant.py`). Consequence: do not share a "text child == U+FFFC" assumption between browsers.
- Interface mask (`GetGTypeInterfaceMask`, `ax_platform_node_auralinux.cc:2383`): everything except images/video gets `Text`+`Hypertext`; `Hyperlink` for links and for all non-text nodes; `Document` only for platform documents per source. [live] the Chrome for Testing binary here advertised `Document` on all 353 nodes, which disagrees with the source; either the checkout is older than the binary or interfaces are reported differently. Treat interface lists as unreliable and prefer roles.
- Landmarks are a distinct `landmark` role with the ARIA name in `xml-roles` (`ax_platform_node_base.cc` object attributes). Live attributes: `live`, `container-live`, ... are emitted (`ax_platform_node_base.cc:1171`).
- `Cache.GetItems` returns frames only (4 items), same as Firefox.
- Tree can be **rebuilt rather than modified** on navigation; `kDocumentTitleChanged` can fire on the old root, so Chromium also handles `kLoadComplete` for title (`ax_platform_node_auralinux.cc:4207-4215`).

### 2.3 Text and offsets [src]

- Offsets are **Unicode code points** (`UTF16ToUnicodeOffsetInText`, `UnicodeToUTF16OffsetInText`, `ax_platform_node_auralinux.cc:1070-1180`), the opposite of Firefox's UTF-16-with-BOM. gorca's text layer must carry the browser kind (or normalize at ingestion to its own canonical unit).
- Line boundaries need `kInlineTextBoxes`; a known bad return exists: `GetTextAtOffset` for LINE can return `start=end=-1` at a valid offset (Orca logs "Chromium returned invalid line boundary", crbug 536662914, `refs/orca/src/orca/ax_text.py:496-512`).
- `AtkDocument` selection API (`get_text_selections`/`set_text_selections`, ATK >= 2.52) is implemented (`SupportsAtkDocumentTextSelections`), allowing cross-node selection. Firefox does not.

### 2.4 Events [src]

`browser_accessibility_manager_auralinux.cc` (`FireGeneratedEvent`, `FireSourceEvent`) and `ax_platform_node_auralinux.cc`.

- `load-complete` via `kLoadComplete` plus `state-changed:busy` via `kLoadStart` / `BUSY_CHANGED` (`FireLoadingEvent`); a code comment concedes that "timing and other issues" can make the busy state still set at load-complete.
- **`text-caret-moved(0)` is overloaded as a "scrolled to anchor" signal** (`OnScrolledToAnchor`, line 3664-3671). A client that treats every `text-caret-moved` as caret movement will misfire on hash navigation. Real caret events go to the *editable root or document*, offset in code points (`EmitCaretChangedSignal`, `FindEditableRootOrDocument`).
- **Text change events are synthesized only for editable nodes and live-region nodes** (`UpdateHypertext`, line 4268-4304: returns early unless editable or `IsInLiveRegion()`); static page text changes produce **no** `text-insert/remove`. They use a common-prefix/suffix diff of old and new hypertext (`ComputeHypertextRemovedAndInserted`). So a Chromium page whose non-live text changes is silent at the ATK level; only `children-changed` (subtree created / will-be-deleted) fire. The mirror must be updated from children-changed + re-query.
- `children-changed::add` on `SUBTREE_CREATED`, `::remove` on subtree-will-be-deleted; ignored nodes never emit (`GetData().IsIgnored()` check).
- ARIA notifications use `notification` signal when ATK >= 2.50, else fall back to `text-insert` + `children-changed::add` on a (possibly extra) announcement node (`OnAriaNotificationPosted`, `FireAriaNotificationEvent`).
- Many generator events are explicitly **unused on Linux**: `LIVE_REGION_CHANGED/CREATED/NODE_CHANGED`, `STATE_CHANGED`, `POSITION_IN_SET_CHANGED`, `ROW_COUNT_CHANGED`, `OBJECT_ATTRIBUTE_CHANGED`, `SCROLL_*` (list at `browser_accessibility_manager_auralinux.cc:399-430`). Live-region semantics on Linux Chromium come from the text-insert synthesis above plus attributes.
- `window:activate` / focus events are postponed until AT-SPI is "ready" (`PostponeEventsFor`, `RunPostponedEvents`), so early events can arrive late and in a burst.

### 2.5 Known Chromium issues [src]

- crbug 536662914: invalid line boundary result (above).
- Orca documents "omnibox popup closed then re-opened: cannot ascend to the frame, parents claim 0 children" (`ax_object.py:226-250`, no bug number: "TODO: file a bug").
- Orca disables some behavior for Electron apps via toolkit name `Chromium` plus app name not in known browsers (`document_presenter.py:170-190`): Electron apps look like Chrome but lack browser chrome; gorca must distinguish (same toolkit string).
- Chromium's own code cites `crbug.com/40549424` for ATK-version fallbacks.

### 2.6 Firefox vs Chromium at a glance

| Aspect | Firefox | Chromium |
|---|---|---|
| ATK layer location | parent process, RemoteAccessible cache | browser process, AXTree mirror |
| Text leaves as nodes | no (inline in parent text) | yes (`static`) |
| Offsets | UTF-16 units (+U+FEFF after astral chars) | code points |
| Text-change events | all text | editable and live regions only |
| Caret event target | focused hypertext | editable root or document; offset 0 = scroll to anchor |
| Cache warm-up | lazy domains, first query may be empty | modes on/off (`AXMode`) |
| Screen reader detection | none | `/proc` scan for `orca` |
| Document selection API | no | yes (ATK >= 2.52) |
| `Cache.GetItems` for web content | chrome UI only | frames only |
| Live region signal | none (attributes) | none (attributes), plus `notification` |
| Native Newton support | none found in `accessible/` | none found in `ui/accessibility` |

---

## 3. Browse mode prior art

### 3.1 NVDA virtual buffers [src `refs/nvda`]

- **Where the data lives:** the buffer is built **inside the target process** (`nvdaHelper/vbufBase/backend.cpp`, injected helper; `execInThread(renderThreadID, ...)`), by walking the browser's IAccessible2 tree with **in-process COM calls** (`vbufBackends/gecko_ia2/gecko_ia2.cpp::fillVBuf`, recursion at lines ~1115-1250 using `get_accChildCount`/`AccessibleChildren`, `IA2` attributes and `paccText->get_attributes`). No per-node cross-process cost. The Python side (`source/virtualBuffers/__init__.py`) only holds a handle and calls into it (`VBuf_*`).
- **Data model** (`vbufBase/storage.h`): a flat **text buffer** (the page as text with `U+FFFC`-style placeholders) plus a tree of **control-field nodes** (attributes: role, states, level, ids) and **text-field nodes**, ordered by offset. Queries: `locateTextFieldNodeAtOffset`, `locateControlFieldNodeAtOffset`, `findNodeByAttributes(offset, direction, attribs, regexp)`, `getLineOffsets`, `getTextInRange(useMarkup)`.
- **Quick navigation** is `VBuf_findNodeByAttributes` (`virtualBuffers/__init__.py:_iterNodesByAttribs`, `gecko_ia2.py:_searchableAttribsForNodeType`): node type -> attribute predicate (role set, states, `level:N`, `table-layout`, editable) evaluated by a linear scan from the current offset, in memory, no IPC. Forward/back/up directions. `formField`, `landmark`, `link` (linked vs visited vs unvisited) are different predicates.
- **Incremental update:** events call `invalidateSubtree(node)` (`backend.cpp:invalidateSubtree`): it climbs to the nearest ancestor that `requiresParentUpdate`, merges into `pendingInvalidSubtreesList` (drops descendants of an already-invalid ancestor), and arms a **100 ms timer** (`requestUpdate`, `SetTimer(..., 100, ...)`). On expiry `update()` re-renders each invalid subtree into a temporary buffer and **replaces** the subtree, reusing unchanged nodes (`allowReuseInAncestorUpdate`, `VBufStorage_referenceNode_t`). So: dirty-set with ancestor-dominance coalescing, fixed debounce, subtree replacement with node reuse. No fine-grained diff.
- **Load:** the first build is asynchronous (`VirtualBuffer.loadBuffer` thread, `isLoading`, "Loading document..." message after 1 s); if the buffer is empty it waits for `documentLoadComplete` (`_loadBufferDone`, `event_documentLoadComplete`) and then runs the initial say-all/announce. It remembers the last caret position per document (`documentConstantIdentifier`) across reloads.
- **Caret model (`browseMode.py`):** the browse-mode caret is **virtual**, owned by the buffer. App caret events are ignored while in browse mode (`event_caret` only forwards `if self.passThrough`, line 1921). Moving the virtual caret calls `scrollIntoView`, and sets app focus only for focusable targets (`_set_selection`, 1940-1990, `_shouldSetFocusToObj`); focus events in the app move the virtual caret (`event_gainFocus`). `shouldPassThrough` auto-switches browse/focus mode by role/state (editable, combobox, application), plus sound cue (`browseMode.wav`/`focusMode.wav`). This sync model is the heart of the "feel": the user's keystroke never waits for the app.
- **Speech from fields:** `speakTextInfo` diffs the control-field *ancestor stack* of the old and new position and speaks only entered/left containers (list, table, landmark), then the content. Quick nav items announce their own field plus context.

### 3.2 Orca [src `refs/orca/src/orca`]

- Orca has no buffer. Web scripts live in `scripts/web/` (script.py 1.8k lines, script_utilities.py 3.5k), toolkit shims in `scripts/toolkits/{Gecko,Chromium,...}`. Caret navigation (`caret_navigator.py`) and say-all (`say_all_presenter.py`) call `get_line_contents_at_offset`/`get_next_line_contents` (above) that compute everything live over D-Bus, with only a small per-script content cache (`LINE_CONTENTS`) and `AXCacheManager`.
- Structural navigation: `StructuralNavigator` (2.4k lines): per-type "get all" = `AXCollection.get_all_matches` (`Collection.GetMatches`) over the document root each time; objects are then compared by index (`_get_object_in_direction`). 40+ navigation types (headings by level, landmarks, form fields, tables with cell-based navigation, live regions, large objects, clickables).
- Live regions: `live_region_presenter.py` has a priority heap (assertive before polite), duplicate suppression, politeness overrides per object; it reads `container-live` from object attributes (line ~426) and subscribes to `busy` changes to wait for the update to finish (`handle_busy_changed`).

### 3.3 What makes NVDA's version feel better [inferred from the above]

1. **Local data**: every navigation command answers from process-local memory (latency independent of the browser). Orca's answer depends on Firefox main-thread responsiveness and dozens of round trips.
2. **One consistent coordinate system**: a single flat offset space for the whole page, so line/word/sentence, selection, find, and say-all are string operations on one buffer; no per-object hypertext offset translation.
3. **Virtual caret decoupled from app focus**, with explicit browse/focus mode and auto-switch.
4. **Predictable invalidation**: 100 ms debounce, subtree replace, caret position kept across rebuilds.
5. **Rich built-in element index** (`findNodeByAttributes`) incl. visited links, layout tables, annotations, and an elements list dialog.
6. **Field-diff speech** (announce only changed containers).
7. **Page-load behavior**: waits for load complete, one clean "start reading" moment, remembers position.

---

## 4. What gorca needs: requirements for the virtual document model

### 4.1 Principles

1. **The mirror is the source of truth on the hot path.** No AT-SPI call may be on the keypress -> speech path (CLAUDE.md latency goal p99 < 15 ms gorca-side). Navigation answers come from the mirror; D-Bus is used to fill and refresh it.
2. **Per-document actor** (one per browser document), owning its mirror, indexes, and a deadline-bound IPC pipeline. A flooding or hung page degrades only its actor.
3. **Browser-specific adapters below, one model above** (like NVDA's backends): adapters normalise offsets (canonical unit: UTF-8 bytes plus a code-point/UTF-16 map), text-leaf style, and event semantics.
4. **The mirror may be stale; never wrong silently.** Every node carries a revision/validity; navigation to a node validates it with one cheap async check before action (focus/scroll), and corrects speech after the fact if it changed.

### 4.2 Initial build

- Trigger: `document:load-complete` (Firefox, Chromium) or first focus/entry into a document, plus a timer fallback (Chromium can deliver load-complete before the tree is populated; NVDA has the same "empty buffer, wait for documentLoadComplete" case).
- Strategy (viewport-first, then breadth): 
  1. Connect P2P (`GetApplicationBusAddress`).
  2. Resolve the showing document (filter `STATE_SHOWING`; ignore background tabs).
  3. **Warm cache domains** with one throwaway text/attribute/extents/table query so Firefox escalates domains once (1.5).
  4. Fetch the focused node's neighborhood first (caret/focus context, ancestors, siblings), then the viewport, then the rest in the background by breadth-first batches with a small number of in-flight requests (pipelined, bounded, per-call deadline).
  5. Make the model **usable at partial coverage**: navigation commands in a not-yet-loaded region fall back to a server-side `Collection.GetMatches` (one round trip) while the build finishes.
- Budget: first-useful in < 300 ms for a typical page (the focus context), full 10k-node page in a few seconds in the background; the sequential-naive baseline of 1.25-1.6 ms/node (12-16 s for 10k) is not acceptable, hence pipelining and P2P. Number to validate by implementation (target: <= 0.3 ms/node amortized).
- Use `Collection.GetMatches` for **bulk discovery of sparse element types** (headings, landmarks, forms, links) during the build; it is one round trip, server-side walk, and gives document order. Cache invariants: it returns references only; attributes still need fetching.
- Fetch per node (minimum): role, states, name, interfaces, `GetAttributes` (tag, level, xml-roles, live-*), child list, parent, index-in-parent; text: `GetText` of the whole hypertext node once (not by line) + `GetNLinks`/links; extents lazily (only when the user needs line-break or layout info), because extents are the most expensive and cold-cache-prone.
- Large nodes: huge text nodes and lists: cap per-call size, stream.

### 4.3 Incremental updates from events

- Event classes to subscribe: `children-changed`, `text-changed/text-insert/text-remove`, `state-changed` (selected, expanded, checked, busy, showing, focused), `property-change` (name, description, value, role), `document:*`, `focus`, `text-caret-moved`, `text-selection-changed`, `notification`.
- Policy: **coalesce per (target, kind), dirty-subtree set with ancestor dominance** (NVDA's rule), debounce window of ~50-100 ms for mutation refresh, **immediate** path for focus/caret (never debounced). Drop superseded events (state toggles that net to no change, text edits superseded by a later refresh of the same node). A bounded event queue per document: when it overflows, mark the whole document dirty and rebuild rather than lose track (storm safeguard; target: sustain 10k events/s as per CLAUDE.md).
- Refresh by **re-fetching the smallest dirty subtree** and replacing it, reusing unchanged nodes by identity (AT-SPI object reference is the stable key; Firefox IDs are stable per Accessible, Chromium node IDs stable per AX node, but Chromium may rebuild the whole tree on navigation).
- Browser-specific rules:
  - Firefox: text-insert/remove arrive for any text, so apply text events to the node's text locally (insert/remove at offset, UTF-16/BOM aware) and verify with a lazy `GetText` of the node when idle; `children-changed::remove` may be missing, so validate with `defunct` handling.
  - Chromium: no text events for non-live static text; on `children-changed` re-fetch the affected parent; run a cheap periodic verification (hash of text of nodes in viewport) after any `children-changed` storm.
  - Both: caret events carry only an offset on the hypertext node; map to the mirror's flat offset.
- Mutation floods (infinite scroll, virtualized lists, ticking tables): subtree replace with reuse, plus **budgeted rebuilding** (cap per-frame work; prioritize the visible/focused region).

### 4.4 Diffing

- Identity is given (object reference), so this is a **keyed ordered-forest diff**, not general tree edit distance: per parent, diff the old and new child key sequences (insert, delete, move via longest-increasing-subsequence), then recursively diff only children whose keys survive and whose revision flag is dirty. Cost O(sum of n log n over dirty parents).
- Text diff: common prefix/suffix (Chromium itself does this: `ComputeHypertextRemovedAndInserted`), upgrade to Myers/rolling-hash only for live regions that are re-rendered wholesale.
- The diff output drives: index updates (4.5), live-region announcements (4.6), caret repair (if the node holding the caret moved or vanished, relocate to the nearest surviving offset in document order, as NVDA does with `bookmark`).
- Math questions in 4.8.

### 4.5 Structural navigation indexes

All indexes are over the mirror, maintained incrementally, with O(log n) or better next/previous from a position:

| Index | Key | Contents | Notes |
|---|---|---|---|
| Document order | node -> ordinal (order-maintenance labels) | enables "next X after node" with a single binary search | must survive inserts without renumbering the page |
| Headings | per level + all | node, level (`level` attribute, not role) | `aria-level` on `role=heading` and native `h1-h6` both land in `level`; skip hidden/ignored |
| Landmarks | role/`xml-roles` | name, role (main, nav, banner, region needs a name) | Firefox: ATK role `landmark` + `xml-roles`; Chromium: role `landmark` + attributes |
| Links | all / visited / unvisited | state `visited` (ATK `STATE_VISITED`) | in-page vs external via `Hyperlink` URI |
| Form fields | editable text, combobox, checkbox, radio, button, slider, spin, switch, listbox | `editable` state, readonly, required, invalid | NVDA's `formField` predicate is the model (3 alternatives) |
| Tables | table, row, cell; header associations; span | grid index (row, col) -> node, with span covers; layout-table detection (`table-layout` attribute / Firefox `layout-guess`) | `Table`/`TableCell` interfaces: fetch spans individually (Mozilla 1862437) |
| Lists / list items, blockquote, separators, images (with/without alt), buttons, iframes, math, clickables, live regions, large objects | by role/attribute | | Orca's 40-type set is the checklist |
| Lines/words/sentences | derived on the flat text | built lazily from text + extents | extents are optional: if absent, use text-flow with `\n` and hard breaks |
| Find / text search | flat text | trigram or Aho-Corasick on the flat text | |

- Navigation across document boundaries: iframes, shadow DOM (browsers expose the flattened tree), `aria-owns` re-parenting (AT-SPI parent/children may disagree with DOM; mirror stores AT-SPI parent).
- Unified **flat text with a node-offset map** (NVDA model): `Vec<Segment{node, start, len}>` or a rope; U+FFFC handled structurally (child inserted in place) so the browser's hypertext offsets map both ways.

### 4.6 Live-region handling

- Classify each insertion/mutation event by the target's effective `container-live` / `live` / `container-atomic` / `container-relevant` / `container-busy` (fetched once per node and cached in the mirror, invalidated by attribute/`property-change`).
- Politeness queue like Orca's: assertive preempts and purges polite; per-region dedupe of identical consecutive text; atomic regions announce the whole region; `busy` suppresses until clear; per-region rate limit with a max backlog (drop oldest polite, collapse bursts into "N updates"), global ceiling for pages that spam.
- Use the **mirror diff** to compute what to say (inserted text for `relevant=additions text`, removed for `removals`), not the raw event string, because Chromium's text events are prefix/suffix approximations and Firefox's include system-origin edits.
- Handle `notification(string, priority)` (`ariaNotify`) as a first-class channel. Treat **`role=alert`/`status`/`log`/`marquee`/`timer`** defaults (the browsers add `live` attributes, Firefox from ARIA role defaults).
- Mute/restore per-page and per-region (Orca's override map).
- Test: floods of 10k inserts/s into one region must keep input latency unaffected (the live region actor drops, never blocks).

### 4.7 Latency budget per operation (gorca-side processing unless noted)

| Operation | Budget (p99) | How |
|---|---|---|
| Keypress -> speech start for structural/caret nav (H, K, arrow) | < 5 ms of the 15 ms total | index lookup + string assembly from mirror; zero IPC |
| Speech text assembly for a node/line | < 1 ms | precomputed per-node name/role/state strings, ancestor stack diff |
| Cancel stale speech on newer event | < 1 ms | preempt, generation counter |
| Event ingest (decode + coalesce) | < 100 us/event, 10k events/s sustained | lock-free queue, batched dirty set |
| Mutation refresh (small subtree, <= 200 nodes) | < 20 ms wall, off hot path, <= 2 ms on-thread CPU | async, bounded concurrency |
| Focus/caret event -> spoken | < 15 ms; the event itself carries node + offset; text from mirror, validated async | never wait on `GetText` |
| Initial usable state | < 300 ms; full 10k nodes in seconds | section 4.2 |
| Any IPC call | hard deadline (e.g. 200 ms default, 50 ms for validation), cancellable, never blocks the document actor loop beyond its own queue | per CLAUDE.md |
| Cold fallback (mirror miss) | one `Collection.GetMatches` round trip, 0.3-5 ms on typical pages (ref numbers above) | used during build only |

Reference IPC costs measured here: ~0.1-0.3 ms p50 and ~1 ms p99 per call; the budget therefore tolerates at most one or two round trips on a hot path, which is why the hot path has none.

### 4.8 What belongs in the core vs a browser extension

Core (gorca, Rust, generic): mirror, diff, indexes, flat text, speech assembly, live-region policy, mode logic (browse/focus), caret sync, event pipeline, P2P AT-SPI client, adapters for Firefox and Chromium quirks (offset units, text-leaf shape, caret overload, cache warm-up).

Browser extension (WebExtension content script, optional, later): things AT-SPI cannot express or exposes expensively:
- **Reliable change notifications for SPA route changes** (History API / `popstate` / `MutationObserver` summary) and **mutation batching info** (a batch-start/batch-end marker so the mirror knows an update settled).
- **Exact visited state, DOM `id`/`class`/`data-*` hooks, shadow DOM/iframe topology** as ground truth, `scrollIntoView`/focus on virtual-caret moves without synthesizing events.
- **Page-level semantic extras**: skip-to-main, heading outline, form error summary, `aria-details`/annotations, visible-text-only filters.
- **Hit testing / reading order hints** for CSS-reordered content.
- Not as a replacement: the AT-SPI path must work with no extension (user requirement: flawless Firefox out of the box). The extension is an accelerator. The core exposes a transport-agnostic "document hints" channel so the extension plugs in as a connector (consistent with the decision log: pluggable connectors, app-specific behavior in extensions).
- Newton: neither browser source mentions it; web content stays on AT-SPI.

Keep out of the core: per-site hacks (go in extensions), Electron app special-casing (toolkit `Chromium`, app name check) can be an app-specific extension.

### 4.9 Questions for the math-researcher

1. **Order-maintenance structure** for document order under insert/delete of subtrees (list-labelling: Dietz-Sleator, Bender et al.): which one gives O(1) compare and amortized O(log n) insert with small constants in Rust; vs. Euler-tour + balanced tree. Requirement: "next heading after node X" in microseconds on 100k-node pages while a mutation storm runs.
2. **Keyed ordered-forest diff** bounds: cost of LIS-based child diff vs full subtree re-fetch; derive the break-even rule (re-fetch ancestor vs patch descendants) as a function of dirty-descendant count and per-node fetch cost. NVDA uses "ancestor dominance" heuristically; is there an optimal cut policy (tree knapsack / minimum-cost covering of a dirty set)?
3. **Coalescing policy** under event storms: window size vs staleness; optimal debounce with immediate-flush priority classes; bounds on delay under M/G/1-like arrivals with a bounded queue (drop-oldest vs coalesce).
4. **Flat-offset text map**: rope or Fenwick over segments for offset <-> (node, local offset) with UTF-16/code-point/grapheme conversions and U+FFFC placeholders; complexity of line/word boundary queries without extents; incremental update cost after a mid-document edit.
5. **Table index**: representation of spans (interval trees / 2D range structures) for `(row, col) -> cell` and header association; incremental maintenance on row insert/delete.
6. **Live-region dedupe**: streaming edit-distance or rolling-hash similarity to detect "same message, timestamp changed" floods; guarantee of bounded work per event.
7. **ANN/vector search** on the flat text (semantic search, "find the section about X") is out of scope for the first milestone; flag only.

---

## 5. Open decisions for the orchestrator

1. **Chromium screen-reader detection.** Options: (a) set gorca's process `comm`/argv0 to `orca` (hack; works with `IsOrcaProcess`; misleading in `ps`), (b) ship a Chrome flag/policy or desktop-file wrapper (`--force-renderer-accessibility`, fragile across Chromium forks), (c) accept `kWebContents`-only mode and compensate. Needs an empirical test: run a stub that mimics gorca's registration and compare tree and event output with and without `comm=orca`.
2. **P2P connections to apps** (measured benefit; zbus supports connecting to an address; check cost of many sockets and the lifetime when apps die).
3. **Canonical text unit** for the mirror (recommend UTF-8 bytes + a per-node code-point/UTF-16 index; adapters convert at the edge).

## 6. Not verified, and recommended next experiments

- Large-DOM behavior (10k, 100k nodes): all numbers above come from pages of 150-350 nodes. Build `tests/web/` pages: huge DOM, SPA route change, infinite scroll, shadow DOM, iframes (same-origin and OOP), `aria-live` floods, tables with spans, astral characters (BOM/UTF-16 check), hash navigation (caret overload). Run them in a private Firefox profile and a private Chromium profile with a private a11y bus (not the user's session).
- Event behaviour (actual signal sequence at load, on SPA route change, on caret movement in caret-browsing mode) was read from source, not captured live. Capture with `Atspi` event listeners against test pages.
- Whether `Cache.GetItems`/eager atk-adaptor traversal stays out of web content on heavy pages, and its cost on Firefox's main thread.
- Which code path in Chromium turns on web accessibility for a non-Orca AT-SPI client (not in the checked-out subset).
- Chrome for Testing advertising `Document` on every node (differs from source).
- Exact cost of Firefox lazy-domain escalation (time to full push on a large document).
- The memory-sourced "known bugs" in 1.7.

## 7. Files

- `<repo>/docs/reports/2026-10-12-web-a11y-browsers.md` (this file)
- `<repo>/tests/web/probes/` read-only probes and README (used for all [live] numbers)
- Key sources: `refs/firefox/accessible/{atk,base,ipc,docs}`, `refs/chromium/ui/accessibility/platform/`, `refs/chromium/content/browser/accessibility/`, `refs/at-spi2-core/atk-adaptor/`, `refs/nvda/nvdaHelper/{vbufBase,vbufBackends}`, `refs/nvda/source/{browseMode.py,virtualBuffers}`, `refs/orca/src/orca/{scripts/web,structural_navigator.py,live_region_presenter.py}`
