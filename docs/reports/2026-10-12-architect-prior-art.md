# Prior art for gorca: Orca failure catalog, Odilia lessons, Newton analysis

- **Date:** 2026-10-12
- **Author role:** architect
- **Feeds:** `docs/adr/0001-core-architecture.md`
- **Method:** reading the code in `refs/` (shallow clones dated 2026-10-12). Every claim cites `refs/...:LINE`. Statements marked *(inference)* are reasoning about the code rather than something the code states directly.

**Versions examined**

| Component | Version |
|---|---|
| Orca | 51.1 (`refs/orca/meson.build:2`) |
| atspi-rs | 0.31.0 (`refs/atspi-rs/atspi/Cargo.toml:14`) |
| zbus | 6.0.0 (`refs/zbus/zbus/Cargo.toml:3`) |
| AccessKit | 0.25.1 / accesskit_consumer 0.39.1 |
| Newton consumer | pinned to AccessKit 0.16.0 / consumer 0.24.0 |
| NVDA | master |

---

## 1. Orca: architectural failure catalog

Orca 51.1 is a modernized code base, with `ax_*` wrappers, `presentation_manager` and `ax_cache_manager`. Its failures are still structural: it is about 120k lines of Python on a single thread making synchronous IPC calls.

Largest modules:

| Lines | File |
|---|---|
| 4347 | `speech_generator.py` |
| 3501 | `scripts/web/script_utilities.py` |
| 2874 | `braille.py` |
| 2825 | `speech_presenter.py` |
| 2416 | `structural_navigator.py` |
| 2122 | `speech_manager.py` |

### F-1 Everything runs on one GLib thread

- `Atspi.event_main()` (`refs/orca/src/orca/orca.py:267`) is the only loop for AT-SPI callbacks, scripts, generators and speech calls.
- Object events are dequeued **one per idle callback**:
  - `GLib.idle_add(self._dequeue_object_event)` (`event_manager.py:745`);
  - `get_nowait()` (`:765`);
  - `rerun = True` while the queue is non-empty (`:798-802`).
- Key commands are deferred to the same loop with `GLib.timeout_add(1, self._handle)` (`input_event.py:755-756`), so a keypress waits behind any slow object-event handler.
- A `threading.Lock` and `queue.PriorityQueue` exist (`event_manager.py:102-106`), but enqueue and dequeue both run on the main thread. *(inference: the lock protects nothing concurrent)*
- Liveness is coupled to the loop. The systemd watchdog ping runs on the main loop (`systemd.py:134-136`). A backlog over 75 events explicitly pings "alive" (`event_manager.py:719-720`). Shutdown uses `signal.alarm(5)` followed by a hard kill (`orca.py:105-113`).
- **Exceptions done right:**
  - Braille runs a BrlAPI worker thread with a 5 s task watchdog (`braille.py:119`, `:286-287`, `:518-554`).
  - Speech-dispatcher callbacks arrive on the speechd thread and are marshalled back with `GLib.idle_add` (`speechdispatcherfactory.py:347-378`).
- A **cache-cleanup daemon thread** shares cache state with the main thread under an `RLock` (`ax_cache_manager.py:246-247`, `:671-676`).

**gorca response:** a non-awaiting Reader actor with biased input-first polling, plus per-app actors (ADR §5).

### F-2 Synchronous D-Bus with an 800 ms default timeout and reentrancy

- Orca never calls `Atspi.set_timeout`, so libatspi's default applies: `static gint method_call_timeout = 800;` (`refs/at-spi2-core/atspi/atspi-misc.c:191`, documented at `:1951-1952`).
- libatspi's `dbind_send_and_allow_reentry` either blocks in `dbus_connection_send_with_reply_and_block`, or **spins a nested `g_main_loop_run`** while it waits (`refs/at-spi2-core/dbind/dbind.c:66-130`). Event callbacks can therefore re-enter Orca in the middle of a handler.
- **Hang detection happens after the fact:**
  - libatspi pings the peer after a `NoReply` (`atspi-misc.c:1222-1245`), then fails fast with "The process appears to be hung." (`:1270-1274`).
  - Orca recognizes this by **string matching** (`ax_object.py:351-353`) and marks the app hung for only `_HUNG_TIMEOUT_SECONDS = 1.0` (`ax_object.py:50`, `:147`).
  - So the first call to a hung app costs 800 ms on the only thread. After the 1 s marker expires, the next call blocks again. *(inference)*
- **Error classification is string matching** on GError messages: "Object does not exist at path", "The application no longer exists" and a regex (`ax_object.py:337-363`).
- **`is_dead()` is a live round trip:** `Atspi.Accessible.get_name(obj)` (`ax_object.py:1063-1066`). It runs for every dequeued event (`event_manager.py:1051`) and on every focus change (`focus_manager.py:345`, `:355`).
- **Filtering before enqueue is itself IPC.** `_ignore` calls `get_role` (`event_manager.py:670`), `get_state_set` (`:400`), `get_attribute(..."live")` (`:422`) and `get_application` (`:439`). Script selection reads the `toolkit` attribute (`event_manager.py:735`, `script_manager.py:138`), and `_get_priority` adds role checks (`:203-212`). Every event in a storm costs several round trips before any dedupe happens.
- **Toolkit name is not cached:** `get_application` plus `get_toolkit_name` on every call (`ax_object.py:163-174`).
- **The key path does IPC:** `can_be_active_window` runs on every key press, and on failure falls back to a desktop walk (`input_event_manager.py:151-157`, `ax_utilities.py:283-285`).

**gorca response:**
- Deadlines on every request (ADR §5.4).
- Typed `BackendError` instead of string matching.
- An `AppHealth` state machine whose `Hung` state persists until a probe succeeds, not for a fixed 1 s.
- Coalescing uses only event payload data, never IPC.

### F-3 Event queue: global priority heap, O(n) obsolescence, ad-hoc spam filters

- The queue is an **unbounded** `queue.PriorityQueue(0)` keyed by `(EventPriority, counter, event)` (`event_manager.py:102-104`, `:741`), with eight priority levels (`:61-71`).
- **Priority reorders causally related events.** Example: an invalid-entry state change is demoted so other changes are presented first (`:226-229`). *(inference: no per-source FIFO guarantee)*
- **Obsolescence is decided at dequeue time** (`_is_obsoleted_by`, `:244-335`):
  - newest-per-`(type, hash(source))` (`:248-263`);
  - for focus among siblings, the queue is copied under the mutex and scanned in reverse (`:299-307`), with `AXObject.get_parent(...)` called per candidate (`:290`). A queue of n events can cost 2n D-Bus calls.
- **The spam filter is keyed only by event type and app.** A second same-type event from the same app within 100 ms is dropped regardless of its source (`:438-441`).
- **Hard-coded drops:**
  - text inserts over 5000 or 1000 chars (`:361`, `:642`);
  - name changes on 14 roles (`:503-522`);
  - `showing` except for 8 roles (`:565-589`);
  - `children-changed` from non-active apps (`:483-490`);
  - a Caja-specific comment about an "unbelievable number of active-descendant-changed" events (`:406-410`).
- **Pausing the queue discards the whole queue** (`:177-180`, `:661-667`).
- **"Is there a queued event?" lookahead is stale by construction.**
  - `record_queued_event` keeps the latest event per type forever (`script.py:61-66`, `:73-74`).
  - Scripts treat it as "pending", e.g. `web/script.py:585`.

**gorca response:** a per-app, keyed, order-preserving coalescer with explicit supersession rules (R-1 to R-4), bounded memory and a `Flooded` mode. Drops become dirty bits, not silent losses.

### F-4 Caching: global flushes and distrust of the platform cache

- The Orca cache key is `hash(obj)` (`ax_cache_manager.py:56-59`), with a 60 s default interval clear (`:53`) and a 2 s generator cache (`generator.py:127`).
- **Global flush on structural events.** `clear_all_cache_now` (`ax_utilities.py:230-237`, `ax_cache_manager.py:421-431`) runs on every `children-changed` add or remove, on expansion and on attribute changes:
  - `scripts/default.py:408`, `:414`, `:576`, `:646`;
  - `web/script.py:645`, `:1040`, `:1111`, `:1199`.

  In a DOM mutation storm, every cache is wiped on every event.
- **libatspi's cache is cleared on every focus change** "because too many apps and toolkits fail to emit the correct accessibility events" (`focus_manager.py:323-326`). The clear is recursive for table cells.
- **28 `clear_cache` call sites**, most of them toolkit bug workarounds: `default.py:730`, `toolkits/gtk/script.py:92`, `web/script.py:1464` (Gecko bug 1867044) and `web/script.py:1524`.
- **"Stable tree" scopes.** The cache is trusted only within one presentation (`presentation_manager.py:519`), never for editable or terminal objects (`:551-553`). The code itself says: "As a general rule, the tree should not be considered stable" (`ax_cache_manager.py:797-800`).

**gorca response:**
- A per-node `Freshness` state machine (ADR §3.3).
- Invalidation that is scoped and driven by events.
- Unreliable toolkits get **connector-level normalization or an extension policy** ("always refetch focus states for toolkit X"), not global flushes in the core.

### F-5 App and toolkit hacks spread through generic code

- **Script selection runs on every event at enqueue** (`event_manager.py:735`). It goes:
  1. normalized app name plus an alias map (`script_manager.py:105-118`);
  2. the toolkit attribute (`:134-139`), then the application toolkit name (`:201-203`);
  3. a role-based terminal script (`:141-146`, `:250-257`).

  Per-object toolkit scripts override app scripts (`:296-304`).
- **Script inventory:**
  - 10 app scripts (`scripts/apps/__init__.py`);
  - 6 toolkit scripts: Chromium, Gecko, Qt, Servo, WebKitGTK, gtk (`scripts/toolkits/__init__.py:3`);
  - web, terminal, switcher and sleepmode.
- **Toolkit and app name checks inside generic code:**
  - Gecko bogus objects (`ax_object.py:183-188`);
  - Qt broken ancestry (`:214-216`);
  - Chromium omnibox (`:236`);
  - `app_name != "soffice"` (`ax_object.py:444-450`; `spellcheck_presenter.py:355`, `842`, ...);
  - GTK3 terminal byte offsets (`ax_utilities_text.py:1374-1377`);
  - mutter-x11-frames (`event_manager.py:351`, `:451`);
  - an Electron suspect-apps list (`ax_utilities.py:302`);
  - Chromium focus (`focus_manager.py:405`).
- **Causality is guessed from the last input event:** 167 call sites of `last_event_was_*` predicates (`input_event_manager.py`).

**gorca response:**
- The core never branches on toolkit or app name.
- `AppInfo` is resolved **once per app**.
- Extensions are matched once per app and bound to the app actor.
- Input causality is explicit: every Reader message carries the epoch and the "caused-by key" id.

### F-6 Latency sources in speech and presentation

- **Each speak is preceded by 5–7 blocking SSIP round trips** that re-apply voice parameters: language (twice with a dialect), voice, rate, pitch, pitch range and volume (`speechdispatcherfactory.py:243-257`, `:304-329`). The speechd Python client then waits on a semaphore (`refs/speech-dispatcher/src/api/python/speechd/client.py:296`).
- **Each interrupt health-probes the server.** `interrupt_speech`, then `_get_server()`, then `is_responsive()` spawns a thread and waits up to 2 s for a `get_output_module` round trip (`speech_manager.py:463`, `:985`; `speechdispatcherfactory.py:630-672`). This runs on every focus change through `presentation_manager.py:86-92`.
- **Cancel is just another blocking SSIP command** with an inline reconnect (`speechdispatcherfactory.py:201-217`, `:388-390`).
- **Sleeps on the main thread:**
  - up to 200 × 10 ms in `spiel.py:551-559`;
  - 3 × 50 ms in `ax_event_synthesizer.py:425-426`;
  - 1 s in `notification_presenter.py:351`.
- **Generators:**
  - 211 `_generate_*` methods for speech and 148 for braille;
  - speech, braille and sound are generated separately for the same object (`presentation_manager.py:520-542`);
  - ancestors are generated recursively (`speech_generator.py:678`, `:1122`).
- **Double-speech avoidance is heuristic:**
  - a post-focus snapshot of about 10 IPC calls (`ax_utilities_event.py:437-476`);
  - 1.0 s time windows (`presentation_manager.py:124-142`).

**gorca response:**
- The speech actor caches voice state and sends only diffs.
- Cancellation is local, because gorca's own queue keeps one utterance in flight.
- There are no health probes on the hot path; the connection state machine handles health.
- Generators produce structured segments once, and speech and braille share them.

### F-7 Focus state is imperative, with no epoch

- Focus state is plain fields (`focus_manager.py:60-71`).
- `set_locus_of_focus` (`:306-372`) does the following in sequence: clears the AT-SPI cache, switches scripts, checks liveness over IPC, assigns, presents, then snapshots over more IPC.
- 69 call sites in 20 files set focus imperatively.
- **No focus generation or epoch.**
  - Stale-event detection relies on `is_dead` and on the stale lookahead from F-3, e.g. `web/script.py:583-588`.
  - The active window is reconciled lazily: on every key press, and through a 2.5 s `_on_no_focus` timeout that is re-added on every queue drain and never cancelled (`event_manager.py:800`).

**gorca response:**
- A `FocusState` enum with explicit transitions.
- A monotonic `Epoch` that every query and utterance carries.
- Focus is written by exactly one owner, the Reader. Navigation produces *intents*.

### Contrast: NVDA patterns worth adopting

- **Speech validity at push time.** `FocusLossCancellableSpeechCommand._checkIfValid` (`refs/nvda/source/eventHandler.py:186-218`) is evaluated when the speech manager pushes an utterance (`refs/nvda/source/speech/manager.py:483-500`).
- **Priority queues inside the reader.** One queue per priority, with preemption and resumption (`speech/manager.py:115-180`, `:382-445`).
- **A pending-event counter** per name, per object and per (name, object) (`eventHandler.py:33-106`). It answers "is a newer event for this object queued?" in O(1).
- **A watchdog.** `MIN_CORE_ALIVE_TIMEOUT = 0.5` and `NORMAL_CORE_ALIVE_TIMEOUT = 10` (`refs/nvda/source/watchdog.py:47-56`), plus cancellable cross-process calls (`:406`).
- **In-process virtual buffers** (`refs/nvda/nvdaHelper/vbufBackends/gecko_ia2`). We cannot replicate them on Linux, so we must compensate with chunked, prefetched document building.

---

## 2. Odilia and atspi-rs: lessons

Odilia is "Beta … absolutely not production ready" (`refs/odilia/README.md:11-14`). Its last commit is 2026-05-28. It is maintained, but its core features remain placeholders.

### What worked

- **The crate split:** binary, cache, common, input, tts, notify (`refs/odilia/Cargo.toml:4`). A `CacheDriver` trait allows a test backend (`cache/src/lib.rs:248-281`).
- **A dedicated pump task** forwards `atspi.event_stream()` into a bounded channel of 10,000 with `try_send`, dropping events rather than blocking (`odilia/src/main.rs:247-268`). The comment explains why: zbus's `MessageStream` buffers 64 messages and *hangs* if it is not polled.
- **Bulk prefetch** through `Cache.GetItems` when an app is first seen (`cache/src/lib.rs:531-546`). The idea is right; the implementation is buggy (see below).
- **Input that works on Wayland** through evdev (`rdev` with grab) in a separate process. Permissions come from `input` group membership (`input-server-keyboard/src/lib.rs:1-5`, `scripts/setup_permissions.sh`).
- **Shutdown** through a cancellation token raced against each future (`main.rs:80-88`).
- **Clippy discipline:** the main crates deny `clippy::unwrap_used` (`main.rs:1-9`).

### What stalled it

1. **No timeouts anywhere and no per-app isolation.**
   - A single serial cache actor on a blocking thread (`main.rs:283`, `cache/src/lib.rs:110-137`) serves every app, and each miss costs about 12 concurrent D-Bus calls (`lib.rs:686-722`).
   - Event dispatch is serial: each event is awaited to completion (`odilia/src/tower/handlers.rs:83-95`).
   - One hung app therefore stalls everything. This is F-2 in async clothing.
2. **Cache correctness bugs that block real use:**
   - Bulk prefetch writes every item under one key with `or_insert` (`cache/src/lib.rs:162-168`, `:546`).
   - `RemoveAccessible` removes the emitter instead of `node_removed` (`cache/src/event_handlers.rs:388`; cf. `refs/atspi-rs/atspi-common/src/events/cache.rs:114-116`).
   - `ChildrenChanged` can panic on an out-of-range index (`event_handlers.rs:286-293`).
   - `TextChanged` mixes byte offsets with character offsets (`event_handlers.rs:237-238`).
   - Cache events are never subscribed (`main.rs:222-223`).
3. **No state modelling.**
   - The `Focus` / `Browse` mode lives only in the keyboard process (`input-server-keyboard/src/lib.rs:784-791`). The main process just announces it (`odilia/src/handlers/input.rs:7`).
   - Structural navigation speaks "Navigate to {role}, {dir}" (`handlers/commands.rs:61-64`). Its traversal helper has a bug that prevents descent (`cache/src/accessible_ext.rs:258`).
4. **Speech without cancellation.**
   - There is no SSIP `CANCEL` anywhere. The "stop speech" command *speaks* "Stop speech" (`handlers/commands.rs:56-58`).
   - One utterance is three separate channel sends (`commands.rs:11-18`), which concurrent handlers can interleave.
5. **Framework over product.** A tower-based handler framework (`odilia-tower`, `tower-iter`) got effort, and its comments no longer match the concurrency (`handlers.rs:67-71` vs `tower-iter/src/service_set.rs:100`). *(inference: abstraction investment ahead of a working vertical slice)*
6. **IPC protocol hygiene.** The input socket has no framing and panics on invalid UTF-8 (`input/src/lib.rs:218-253`, `:244`).

### atspi-rs: reuse, wrap, avoid

- **Reuse:**
  - `atspi-common` types: `ObjectRef` (whose hashing ignores owned vs. borrowed, `object_ref.rs:469-499`), `Role`, `State`, `InterfaceSet`, `RelationType`, event structs and match-rule constants.
  - `atspi-proxies` interface definitions.
- **Wrap:**
  - **Decoding can panic.** `ObjectRef` deserialization `assert!`s a non-empty name (`refs/atspi-rs/atspi-common/src/object_ref.rs:442`), so a malformed signal from any app can crash the reader. Decode with a validating pre-check, or behind `catch_unwind`, in the connector demux.
  - **Event parsing allocates.** `TryFrom<&zbus::Message> for Event` (`atspi-common/src/events/event_wrappers.rs:837-881`) produces owned `String`s and `ObjectRefOwned` (`object_ref.rs:212-220`). At 10k events/s that is acceptable, but B-2 should measure a borrowed fast path that only extracts the coalescing key.
  - **No connection configuration.** `AccessibilityConnection` builds its zbus connection internally (`atspi-connection/src/lib.rs:103-122`). Build our own `zbus::Connection` and construct proxies on it.
- **Avoid:**
  - `remove_match_rule` calls `add_match_rule` (`atspi-connection/src/lib.rs:262`). Issue `AddMatch` / `RemoveMatch` ourselves.
  - The default `p2p` feature (`atspi/Cargo.toml:19`) calls every registered app in sequence at startup, with no timeout (`atspi-connection/src/p2p.rs:304-337`). Disable it, or run discovery per app with deadlines.
- **zbus facts that constrain the design:**
  - The only timeout is connection-wide: `Builder::method_timeout` (`refs/zbus/zbus/src/connection/builder.rs:465-474`), applied in `Connection::call_method` (`connection/mod.rs:312-316`).
  - `Proxy::call_with_flags` **bypasses** it (`proxy/mod.rs:871-899`).
  - `DEFAULT_MAX_QUEUED = 64` (`connection/mod.rs:52`) can be changed with `Builder::max_queued` (`builder.rs:350`). A `MessageStream` must be polled continuously (`message_stream.rs:24`).
  - **Design rule:** every call in the connector goes through one `call(deadline, ...)` helper that wraps `tokio::time::timeout`. A lint or `clippy::disallowed_methods` rule bans direct proxy calls outside that helper. Raise `max_queued` to 4096 or more, and keep the demux loop non-awaiting.

---

## 3. Newton: consumer API, data flow, connector requirements

Newton consists of `refs/newton/*` (consumer, atspi-compat, atspi-adapter and test-consumer) plus branches of mutter, gtk, orca and libadwaita. The last activity was 2024-06-29. All clones are shallow. Two pieces are **not in refs**: the Wayland protocol XML (`a11y-v1.xml` in Matt Campbell's wayland-protocols fork) and the toolkit-side `accesskit_newton_adapter`.

### 3.1 Transport (current design, June 2024)

**App → compositor** uses a staging Wayland protocol, built from `['a11y','staging','v1']` (`refs/newton/mutter/src/meson.build:1094`):

- `wp_a11y_manager_v1.get_a11y_surface(wl_surface)` gives one `wp_a11y_surface_v1` per surface (`meta-wayland-surface.c:1903-1932`).
- The compositor sends `updates_wanted(new wp_a11y_updates_v1)` per consumer sink (`:1766-1782`).
- The app replies with `send(fd)` (`:1714-1740`). The compositor sends `unwanted` when the consumer leaves (`:1590`, `:1702`).
- Actions arrive at the app as `action_request(fd)` (`:1806`).
- **Updates are tied to `wl_surface.commit`** (`:955-964`). A second `send` before commit drops the earlier fd (`:529-546`, `:1733`). The app must therefore produce at most one coalesced update per commit. GTK forces a commit (`refs/newton/gtk/gtk/a11y/gtkaccesskitroot.c:241`).

**Compositor → AT** uses D-Bus on the *session* bus. The service is `org.freedesktop.a11y.Manager` at `/org/freedesktop/a11y/Manager` (`refs/newton/mutter/data/dbus-interfaces/org.freedesktop.a11y.xml`; owned at `meta-a11y-manager.c:528-539`):

- Methods: `GetKeyboardFocus() -> u`, `RequestSurfaceUpdates(u, o sink)`, `DoAction(u, h fd)`, `GrabKeyboard`, `UngrabKeyboard`, `SetKeyGrabs(au, a(uu))`.
- Signals: `KeyboardFocusChanged(u)`, `PointerMoved(u, d, d)`, `SurfaceDestroyed(u)`, `KeyEvent(b, u, u, u, q)`.
- The AT exports `org.freedesktop.a11y.UpdateSink.Update(h fd)` and emits `Closed`. Mutter calls `Update` on the AT (`meta-a11y-manager.c:720-735`).

**Payload:** serde **JSON** of AccessKit `TreeUpdate`, written to a pipe whose fd is passed over D-Bus. The XML says: "TODO: Finalize the serialization format." The consumer reads to EOF and runs `serde_json::from_slice` (`refs/newton/consumer/lowlevel/src/state.rs:66-81`, `:280`).

**Security: none.**
- No sender checks (`meta-a11y-manager.c:247-336`).
- `KeyEvent` is a broadcast signal for every key, so any session-bus client sees all keys.
- Any client can call `GrabKeyboard`.
- Key snooping works only in the native backend (`native/meta-seat-impl.c:438`).

### 3.2 Consumer-side API (`newton_lowlevel_consumer`)

- `Consumer::new(Box<dyn EventHandler + Send>)` (`consumer/lowlevel/src/lib.rs:28`) creates a tokio current-thread runtime and a zbus session connection on a dedicated thread, `newton-consumer` (`worker.rs:255`, `:329-375`).
- **Callbacks** (`shared.rs:93-100`): `surface_removed`, `focus_gained`, `focus_lost`, `pointer_moved`, `update(surface, TreeUpdate)` and `key_event`. They all run on the worker thread.
- Surfaces are discovered lazily, only through focus or pointer signals (`state.rs:180-224`). `start_updates` registers `/UpdateSink/{n}` and calls `RequestSurfaceUpdates` (`state.rs:98-127`). A per-sink mutex preserves update order (`:59-61`).
- **Tree state belongs to the user of the crate.** `consumer/atspi-compat` keeps `HashMap<u32, accesskit_atspi_common::Adapter>`, one per surface (`atspi-compat/src/lib.rs:58`, `:111-125`).
- **No app identity:** "TODO: the consumer protocol should give us a way of distinguishing between applications" (`atspi-compat/src/lib.rs:54-55`).
- The consumer `unwrap()`s on D-Bus failures. *(not reusable as is)*

### 3.3 Update and event flow to an AT

1. GTK builds the update at frame time: `gtk_accesskit_context_update_tree` runs before render (`gtkwidget.c:11948-11950`), then `gtk_accesskit_root_update_tree` (`gtkaccesskitroot.c:549-561`). That is at most one update per frame.
2. The first update is full (`build_full_update`, `:202-218`). Later updates are incremental, built from a dirty queue (`:426-446`) that `state_change`, `platform_change` and `bounds_change` fill (`gtkaccesskitcontext.c:145-221`).
3. Mutter forwards the fd at commit to every sink (see 3.1). The AT reads the JSON and applies it to `accesskit_consumer::Tree` with `update_and_process_changes(update, &mut impl TreeChangeHandler)` (`refs/accesskit/accesskit_consumer/src/tree.rs:592-638`).
4. **Events are derived by diffing** (`tree.rs:578-583`, `:640-677`): `node_added`, `node_updated(old, new)`, `focus_moved` and `node_removed`. `accesskit_atspi_common` maps them to AT-SPI-like events:
   - states by XOR (`adapters/atspi-common/src/node.rs:604-618`);
   - name, description and value by field comparison (`:620-670`);
   - live regions, where a node with `live` != Off emits an `Announcement` on add or name change (`adapter.rs:72-77`);
   - **text changes by a common-prefix/suffix diff of the entire `document_range().text()`** (`adapter.rs:121-182`). That is O(document) per update, which is too slow for terminals and long documents;
   - caret moves from `text_selection` (`:197-247`).
5. **Node lifetime is implicit:** a node is removed when no parent lists it (`refs/accesskit/accesskit/src/lib.rs:3229-3232`; `tree.rs:353-408`). **Invalid updates panic** inside `Tree` (`tree.rs:88-449`).

### 3.4 Actions

- The AT serializes an `ActionRequest` to JSON in a pipe and calls `DoAction(surface, fd)` (`consumer/lowlevel/src/state.rs:33-52`). Mutter forwards the fd as `action_request` (`meta-wayland-surface.c:1796-1808`).
- If the app has no provider yet, the fd is silently closed (`:1798-1803`). There is **no reply or acknowledgement**.
- GTK handles `DEFAULT`, `FOCUS` and `SET_TEXT_SELECTION` on its main thread (`gtkaccesskitroot.c:295-304`, `gtkaccesskitcontext.c:2010-2123`). Scroll actions are a TODO (`:2123`).

### 3.5 Identity and text

| Concept | Definition | Source |
|---|---|---|
| Surface id | `u32`, a counter local to the D-Bus interface, freed on `SurfaceDestroyed` | `meta-a11y-manager.c:64-124` |
| App grouping | none: no pid, no app-id | |
| `NodeId` | `u64`, unique per tree | `refs/accesskit/accesskit/src/lib.rs:703-715` |
| GTK node ids | a per-root `u32` counter | `gtkaccesskitroot.c:448-452` |
| Text-run ids | `(layout_id << 32) \| start_usv_offset`, so **they change when earlier text changes length** | `gtkaccesskitcontext.c:988-993` |

**Text** lives in `TextRun` leaves (called `InlineTextBox` in 0.16). Each run carries `value`, per-character UTF-8 lengths, word starts, positions, widths and direction (`lib.rs:2384`). `accesskit_consumer` provides `TextPosition` and `TextRange` with character, word, line, paragraph, page and document movement (`refs/accesskit/accesskit_consumer/src/text.rs:308-570`), plus `to_global_utf16_index` and `to_global_usv_index`.

### 3.6 Version drift

| | AccessKit | accesskit_consumer |
|---|---|---|
| Newton consumer | 0.16.0 | 0.24.0 |
| Current refs | 0.25.1 | 0.39.1 |

Breaking changes since 0.16 (`refs/accesskit/CHANGELOG.md`):

- `NodeBuilder` became `Node`, `InlineTextBox` became `TextRun`, `name` became `label`, and `Action::Default` became `Click`;
- multi-tree support adds `tree_id`, and `ActionRequest` now carries `target_tree` and `target_node`;
- `word_lengths` became `word_starts`.

Current serde uses `deny_unknown_fields` and camelCase (`lib.rs:3203-3207`). *(inference: 0.16 JSON will not deserialize into 0.25 types; a pinned decoder or a schema shim is required)*

### 3.7 What a gorca Newton connector needs

1. A zbus proxy for `org.freedesktop.a11y.Manager`, plus an exported `UpdateSink` object per subscribed surface. Owned by `gorca-connector-newton`, with deadlines on every call.
2. **A versioned decoder.** Read the fd to EOF in a blocking-safe task, cap the size (for example 64 MiB), and decode JSON into a pinned AccessKit version.
3. **One `accesskit_consumer::Tree` per surface**, behind `catch_unwind`. A panic drops that surface's tree and resubscribes (`stop_updates` / `start_updates`) to get a full tree. Better: run tree application in a **separate process**, because the input is untrusted JSON.
4. **Our own `TreeChangeHandler`** that emits core `NodeEvent`s directly. The text diff must be **per run**, not over the whole document: a changed run id or value becomes a `TextChanged` on the containing text node, with offsets computed with `TextPosition::to_global_*`.
5. **Local query answering.** `Fetch`, `Children`, `Subtree` and `Text` are answered from the tree as `Fresh`, with no IPC. Actions go out as a fire-and-forget `DoAction`. Success is confirmed only by the next update, so actions need an "expect effect within N ms" timer.
6. **Identity:** `LocalId = (surface_id << 32) | node_low32`. This works while GTK ids fit in 32 bits, but **it is not general**. With current AccessKit we need `(tree_id, node_id)` interning, as for AT-SPI. Text-run ids are unstable, so **text runs are never used as `NodeKey`s for cursor anchoring**: anchor to the text container plus a global offset.
7. **An input source.** `KeyEvent`, `SetKeyGrabs` and `GrabKeyboard` give a complete key path. Expose them through `gorca-input-api` as a `KeySource`, so the Newton connector can also provide keyboard input.
8. **Grouping.** Until Newton has app identity, each surface is its own "app scope", and therefore its own app actor in gorca. That still isolates per surface, which is fine.

---

## 4. Cross-cutting conclusions feeding the ADR

| # | Lesson | Source | ADR section |
|---|---|---|---|
| L-1 | Never do IPC on the input or dispatch path. Coalesce on payload only. | F-1, F-2, F-3 | §5 |
| L-2 | Per-app isolation is mandatory. Odilia's single cache actor reproduces Orca's freeze in async form. | F-2, Odilia #1 | §5.1 |
| L-3 | Every IPC call goes through a deadline helper. zbus's `call_with_flags` and atspi's `p2p` bypass timeouts. | zbus, atspi-rs | §4 |
| L-4 | Treat all app-supplied data as untrusted: atspi `ObjectRef` asserts, AccessKit `Tree` panics. | atspi-rs, Newton | §4, §5.5 |
| L-5 | Focus needs an epoch, and speech needs push-time validity. | F-7, NVDA | §6 |
| L-6 | gorca owns the speech queue. Cache voice parameters; no health probes on the hot path. | F-6, Odilia #4 | §6.3, §7 |
| L-7 | App quirks go into extensions or connector normalization, matched once per app. | F-5 | §8 |
| L-8 | Newton's unit is the surface, not the app. Text diffing must be incremental, and the wire format is unstable. | Newton | §4, §14 |
| L-9 | Build a vertical slice before frameworks. Odilia's tower layer arrived before structural navigation worked. | Odilia #5 | process |
