# ADR-0001: Core architecture — connector-neutral engine, per-app actors, cancellable output

- **Status:** Accepted (2026-10-12). Sign-off on R1–R6 and the performance philosophy is recorded in `docs/decisions.md`. Items in §17.2 are new and still need sign-off; they are marked *proposed* where they appear.
- **Date:** 2026-10-12
- **Deciders:** project owner
- **Supersedes:** none
- **Related:**
  - `docs/reports/2026-10-12-architect-prior-art.md` (Orca/Odilia failure catalog, Newton analysis)
  - `docs/reports/2026-10-12-linux-platform-foundation.md` and `docs/platform/*` (keyboard monitor, broker, VTE, SSIP measurements)
  - `docs/reports/2026-10-12-web-a11y-browsers.md` (Firefox/Chromium AT-SPI behaviour, browse-mode requirements)
  - `docs/adr/0002-multimodal-perception-layer.md` (Accepted 2026-10-12): extends §6 (feature actor, perceive-io), §7.2 (`Find`, `Map` modes), §8.2 (proposed `SpeechCmd::Earcon`), §11 (new crates, dependency rules 8–12), §14 (metrics PM-1…PM-14)
  - `docs/adr/0004-ingress-drain-and-coalescer.md` (Accepted, conditional on its BF-1…BF-4): resolves B-2; §5.3, §6.1, §6.2 and §6.3 amended accordingly
  - `docs/adr/0003-web-document-mirror.md` (Accepted): refines §9 (web mirror, sources, identity), §7.2 (browse/focus `WebMode`), §11, §15 (B-6 arms); its amendment list is in its §2.1
  - `docs/bakeoff/b1-mirror-fill/SPEC.md` (B-1 native mirror fill): its §15 amendments are applied to §4.1 (I-1), §4.2, §4.4, §5.1 and §8.1 (2026-10-13)

## 0. Changelog

| Date | Change |
|---|---|
| 2026-10-12 | Draft (Proposed). |
| 2026-10-12 | **Accepted with amendments.** R1: Orca identity impersonation (bus name, process name), Chrome flag (§5.5). R4 changed: speech is a separate service behind `SpeechClient` with voicego-native and SSIP backends; in-process synthesis and PipeWire tuning dropped; B-3 retired (§8). R6 extended: `SourceLayer` under the web document mirror, identity-reconciliation requirements, bake-off B-6 (§9). New §3, performance and concurrency model: multi-threaded, sharded, single-writer/lock-free-reader state, dedicated hot lanes; Reader, event-bus and per-app bottleneck audit; B-5 upgraded to a measured A/B. Platform facts added: unbounded broker buffering then disconnect, so a dedicated drain is mandatory (§5.3); unicast un-acked `KeyEvent` and re-injection-only pass-through (§7.3); VTE full-screen diffs per frame (§5.6). Open questions P-1…P-6 and W-1…W-4 resolved or narrowed (§16). Final workspace layout (§11). |
| 2026-10-12 | Cross-reference: ADR-0002 (Proposed) adds the multimodal perception layer. It extends §6, §7.2, §8.2, §11 and §14 without changing any accepted decision here; its amendments take effect only when ADR-0002 is accepted. |
| 2026-10-12 | Cross-reference: ADR-0002 accepted; its amendments to §6, §7.2, §8.2, §11 and §14 are in effect. ADR-0003 (Proposed) amends §9, §11 and §15 (list in ADR-0003 §2.1), effective on its acceptance. |
| 2026-10-12 | **B-2 resolved → ADR-0004** (candidate A: raw-socket drain, lock-free per-app ring). §6.3 rewritten: rule H (hard cap), `SlotBudget` with a flat per-app reserve and preemption (IB-7), resource bounds IB-1…IB-11 and audit invariants AU-1…AU-8 are now architectural invariants. §3.2, §3.3, §5.3, §6.1, §6.2 and §15 updated: one event connection, sharded event connections (arm c) not adopted. |
| 2026-10-13 | **B-1 SPEC §15 amendments applied** (orchestrator sign-off; `docs/bakeoff/b1-mirror-fill/SPEC.md`). §4.1: I-1 restated — removal is not final for path-unique toolkits, a successful reply to a request issued after the tombstone revives the key (LR-3); AT-SPI `LocalId` is the path hash, no generation. §4.2: `NodeSnapshot` gains `have` and `confirmed_seq`; `PropMask::PARENT`. §4.4: `Fetching` leaves the published snapshot (in-flight table of the fill driver), `Stale` carries a mask, ordering rules LR-1…LR-4 and the app-wide stale epoch; transition table rewritten, semantics otherwise unchanged. §5.1: `BackendOp::{ChildSnapshots, Ancestry, CacheDump}`, `SnapshotBatch.truncated`, `BackendError::Unresolved`. §8.1: every cache-served presentation is tagged `may_refine` and revalidated once (RV-1). Cache-signal mapping rows: ADR-0004 §2 D-7. |

## 1. Context

gorca replaces Orca. The goals in `CLAUDE.md` are hard constraints:

| Goal | Constraint it puts on the architecture |
|---|---|
| p99 < 15 ms gorca-side keypress → speech start | No synchronous IPC on the keypress path when a cached answer exists. Presentation works from a local snapshot. |
| Never block | Every IPC call has a deadline. No component waits on another without a deadline. |
| 10k+ events/s | Events are coalesced before any expensive work. Ingest is O(1) per event and allocation-light. The socket is drained faster than the broker fills it. |
| Crash isolation | Per-app handling and extensions sit behind a boundary that a panic, trap, hang or flood cannot cross. Speech runs out of process. |
| Backend-agnostic core | The core never names AT-SPI, D-Bus, zbus, AccessKit, CDP or Wayland types. |
| Speed beats frugality | Use all cores. Lock-free and sharded state. Parallel tree builds. Tens of MB of RAM are an acceptable price for latency. |

### 1.1 Prior art

The prior-art report shows that Orca's instability is structural:

- **One thread and synchronous IPC.** libatspi re-enters the main loop while waiting (`refs/at-spi2-core/dbind/dbind.c:66-130`), with an 800 ms default timeout (`refs/at-spi2-core/atspi/atspi-misc.c:191`). Orca calls role and state queries inside event filtering (`refs/orca/src/orca/event_manager.py:654-724`) and on every key press (`input_event_manager.py:129-170`).
- **Ad-hoc event pruning**, no per-app isolation, implicit app logic.

Odilia showed that Rust, zbus and `atspi` are a workable base. Its weak points: no deadlines, one serial cache actor, no speech cancellation. zbus has only a connection-wide `method_timeout`, which `Proxy::call_with_flags` bypasses (`refs/zbus/zbus/src/connection/builder.rs:465-474`, `refs/zbus/zbus/src/proxy/mod.rs:871-899`). atspi-rs `ObjectRef` decoding `assert!`s (`refs/atspi-rs/atspi-common/src/object_ref.rs:442`).

NVDA provides two patterns we adopt: validity-checked cancellable speech (`refs/nvda/source/eventHandler.py:186-218`, `refs/nvda/source/speech/manager.py:483-500`) and a watchdog (`refs/nvda/source/watchdog.py:47-56`). NVDA's in-process virtual buffer (`refs/nvda/nvdaHelper/vbufBase/backend.cpp`) is the bar for browse mode.

Newton (stale since 2024-06) remains a future connector only (R5).

### 1.2 Platform facts that shape this ADR (new)

From `docs/reports/2026-10-12-linux-platform-foundation.md` (LP) and `docs/reports/2026-10-12-web-a11y-browsers.md` (WEB):

| # | Fact | Consequence | Section |
|---|---|---|---|
| F-1 | Mutter authorises `org.freedesktop.a11y.KeyboardMonitor` only for the owner of `org.gnome.Orca.KeyboardMonitor` (`refs/mutter/src/backends/meta-a11y-manager.c`, `docs/platform/org.freedesktop.a11y.KeyboardMonitor.md`). | gorca owns that name, so it cannot run next to Orca. | §5.5, §7.3 |
| F-2 | `KeyEvent` is a **unicast signal with no acknowledgement**. Mutter never waits. Only keys in the grab set are swallowed, and a swallowed key can reach the app only by re-injection (RemoteDesktop/EIS). Windows that inhibit shortcuts get no a11y key events. | Grab-set state machine; re-injection is a fallback; latency of our processing never delays apps. | §7.3 |
| F-3 | dbus-broker on the a11y bus has effectively unlimited quotas. A slow reader is **buffered without bound and then disconnected**, never silently dropped (`docs/platform/dbus-broker-a11y.md`, `src/bus/driver.c driver_forward_broadcast`). Same-uid senders are never throttled. | A dedicated, never-blocking socket drain is mandatory; overload handling happens after reading. Method replies must not share a socket with the event flood. | §5.3 |
| F-4 | VTE rebuilds the whole visible screen every frame (≤ ~60 Hz) and emits one `delete` + one `insert` covering everything between common head and tail; under scrolling output that is the whole screen, ~tens of KB per event (`docs/platform/vte-terminal-accessibility.md`). `g_assert(start_offset >= 0)` aborts the terminal server. `CharacterCount` disagrees with `GetText` length. | Per-node text coalescing to `Replaced`, lazy payload decode, never send negative offsets, never trust `CharacterCount`. | §5.6 |
| F-5 | SSIP via speech-dispatcher: SPEAK→BEGIN p50 0.7 ms idle, but cancel + new message → BEGIN p50 13.1 ms, max 20.5 ms. PipeWire quantum 21.3 ms. | Speech-server choice dominates audible latency. gorca owns the queue and cancels locally. Server-side latency is measured, not engineered here. | §8 |
| F-6 | `Cache.GetItems` returns 168–334 items in 4–5 ms for native apps, but **no web document nodes** (Firefox: chrome UI only; Chromium: frames only). | Bulk fill works for native apps only. Web documents need their own build path. | §5.4, §9 |
| F-7 | AT-SPI per-call cost to browsers: p50 0.1–0.3 ms, p99 ~1–1.6 ms. Naive walk 1.25–1.6 ms/node. P2P (`GetApplicationBusAddress`) cuts p50 by 35–60 %. Firefox answers on its parent main thread. | Pipelined P2P requests. Client-side parallelism helps decoding and indexing, not the server. | §5.4, §9 |
| F-8 | Firefox: offsets in UTF-16 units with U+FEFF after astral chars; lazy cache domains return empty data on first use. Chromium: code points; text events only for editable and live nodes; `text-caret-moved(0)` overloaded for scroll-to-anchor; detects screen readers by `/proc/*/comm == "orca"` or argv element `orca` (`refs/chromium/content/browser/accessibility/browser_accessibility_state_impl_auralinux.cc:43-56`). | Connector normalisation tables per toolkit; R1 process name. | §5.5, §9 |
| F-9 | Apps build trees only when `org.a11y.Status.IsEnabled` is true; Chromium reads it once at start. | gorca sets `IsEnabled` and `ScreenReaderEnabled` at startup, before apps launch where possible. | §5.5 |

## 2. Decision (summary)

1. **Engine plus connectors (R2).** gorca is a library engine plus a thin daemon. Accessibility sources are connectors behind a message-based boundary with a mandatory deadline on every request (§5). Each application is owned by exactly one connector.
2. **Connector-neutral object model.** Identity is a `NodeKey` scoped by connector, app and incarnation. Nodes are immutable snapshots. A stale key resolves to `Gone`, never to the wrong object (§4).
3. **Concurrency model (new, performance philosophy).** Multi-threaded everything. State is **single-writer, many-reader**: each actor owns and mutates its shard and publishes immutable snapshots that any thread reads without locks. Latency-critical loops run on **dedicated hot-lane threads**; bulk work runs on a work-stealing runtime and a CPU pool sized to all cores (§3).
4. **One actor per application, one actor per web document.** App actors own the mirror shard, coalescer and health state machine. Document actors own the browse-mode mirror (§6).
5. **Non-blocking Reader (R3).** One Reader owns user-visible state (focus, mode, navigation, epoch). It never awaits. It reads mirrors lock-free and sends queries as messages. Its per-message work is bounded and O(1) or O(log n). Anything heavier is offloaded (§3.3, §6).
6. **Dedicated socket drains (F-3).** Every D-Bus connection that receives broadcast signals is drained by a dedicated task on a hot-lane thread. It never awaits downstream. Overload is handled after reading, by coalescing and dirty-marking (§5.3).
7. **Speech is a separate service (R4).** gorca owns the utterance queue, epochs and cancellation. It talks to the server through `SpeechClient`, with backends voicego-native (primary) and SSIP (compatibility). No in-process synthesis (§8).
8. **Web documents (R6).** A per-document mirror is built viewport-first. A `SourceLayer` feeds it from AT-SPI, from an own gorca protocol (CDP, WebExtension) or a hybrid of both. Bake-off B-6 decides; the identity-reconciliation requirements are fixed here (§9).
9. **Orca impersonation (R1).** gorca owns `org.gnome.Orca.KeyboardMonitor` and runs with process name `orca` (§5.5).
10. **No Newton runtime dependency (R5).** The connector API is shaped so that Newton can be added later. The crate is not scaffolded (§5.7).
11. **Extensions are WASM components**, with native first-party extensions only if bake-off B-4 requires it (§10).
12. **Workspace layout with dependency rules enforced in CI** (§11).

## 3. Performance and concurrency model (new)

### 3.1 Principles

- **PC-1 Single writer, published snapshots, lock-free readers.** Every piece of mutable state has exactly one owning actor. That actor publishes immutable snapshots (`Arc<NodeSnapshot>` in a concurrent map, or `ArcSwap<DocSnapshot>`), and readers on any thread read them with no lock and no message. This is read-copy-update: writers run in parallel across shards, and readers never contend with them.
- **PC-2 No global event bus.** Ingress is sharded by application at the socket drain. No channel carries all events of all apps to one consumer.
- **PC-3 Serialise only what is inherently serial.** There is one user, one focus, one speech stream. Only the Reader and the speech queue are serial, and both do bounded work per message.
- **PC-4 Hot lanes are dedicated threads.** The input monitor, socket drains, Reader and speech I/O each run on their own OS thread. They are never starved by bulk work on worker threads. Where `rtkit` allows (`org.freedesktop.RealtimeKit1`, as PipeWire uses), they get low `SCHED_FIFO` priority on the RT kernel.
- **PC-5 Bulk work fans out.** Tree builds, reply decoding, diffing, index construction and text verbalisation run on the tokio worker pool (I/O-bound) or a rayon pool (CPU-bound), sized to all cores. Results come back as messages tagged with an epoch or a revision.
- **PC-6 Memory is spent for latency.** Preallocated maps, precomputed per-node speech strings, duplicate indexes, generous socket buffers (`SO_RCVBUF` raised). The guard rail is "no unbounded growth" (M-8), not a small RSS.
- **PC-7 Every queue is bounded, and every bound has a defined overflow action** (§6.2). Unbounded growth is a bug.

### 3.2 Threads and runtimes

| Lane | Threads | Runs | Priority |
|---|---|---|---|
| `input` | 1 | session-bus connection dedicated to `KeyEvent`, key classification, epoch bump on interrupting keys | FIFO low (rtkit) |
| `drain-N` | 1 (one event connection, ADR-0004 D-3) | socket read, framing, header peek, routing into per-app ingress | FIFO low |
| `reader` | 1 | Reader actor | FIFO low |
| `speech-io` | 1 | speech queue state machine, `SpeechClient` socket I/O | FIFO low |
| `watchdog` | 1 | heartbeat monitoring of the lanes above (§6.6) | normal |
| `workers` | tokio multi-thread, `available_parallelism()` threads | app actors, document actors, connector request I/O (P2P connections), extension host, supervisor | normal |
| `cpu` | rayon, `available_parallelism()` threads | parallel decode, diff, index build, terminal screen diff | normal (`SCHED_BATCH` candidate) |

The hot lanes are mostly idle and wake on I/O. Oversubscription is deliberate (PC-6). Bake-off B-5 validates this layout against "everything on one multi-thread runtime" (§15).

### 3.3 Single-threaded bottleneck audit

| Candidate bottleneck | Why it is serial | How it scales | Bound / metric |
|---|---|---|---|
| **Reader** | Focus, mode, epoch and navigation are one user's state. Parallelising them would create order bugs (Orca's and Odilia's reentrancy and lost-ordering failures). | (1) Cache hits are read lock-free from mirror and document snapshots (PC-1), so a hit costs no message round trip. (2) Hot-path presentation (focus, one nav step) runs inline as a pure function over snapshots. Bulk presentation (say-all chunks, tables, live-region diff text) runs as `PresentJob`s on `cpu`, and the results go **straight to the speech lane** with their epoch, without passing back through the Reader. (3) Index lookups (next heading) are O(log n) on immutable document snapshots. (4) Inbound traffic is only what is user-relevant after app-side coalescing and filtering: focus-app events, announcements, key intents, query replies. | Per-message p99 < 100 µs. Occupancy < 10 % of one core under the M-3 storm (M-14). |
| **Event bus** | There is none (PC-2). | The drain routes by sender to per-app ingress. Sharding heavy senders onto their own event connections (B-2 arm c) was measured and not adopted (ADR-0004 §4): one drain saturates at ~6M messages/s. | M-3, M-11 |
| **Socket drain** | A D-Bus connection is one byte stream; framing is sequential. | The drain does header-only work (about 0.3 µs per message measured in B-2). Body decoding is deferred to app actors on `workers`. Method calls use separate connections (§5.3), so replies never queue behind events. | M-11 |
| **App actor** | One per app; it owns that app's coalescer and mirror shard. | App actors run in parallel on `workers`. Heavy apps are split: a browser app actor forwards per-document batches to document actors, which run in parallel. Per-event cost in the app actor is O(1) (coalescer drain plus routing). Reply decoding and subtree builds fan out to `cpu`. | M-3: 100k ev/s on one app does not move other apps' M-1. |
| **Document actor** | One per document; it owns the document mirror. | Initial build: pipelined requests (AT-SPI) or bulk snapshot (own protocol), parallel decode on `cpu`, parallel index build per index type, then one snapshot publish. Updates: dirty-subtree refresh in parallel, applied in one batch. | M-9, M-15 |
| **Speech queue** | One audio stream. | Bounded O(1) work per command. Verbalisation (symbols, pronunciation) runs before enqueueing, on the producer's thread or `cpu`. | M-5 |
| **Coalescer** | Per-app, written by the drain, drained by the app actor. | Bounded lock-free MPSC ring per app, fold on the consumer (ADR-0004); push is one CAS. | M-3, B-2 T7 |

### 3.4 The epoch as a lock-free cancellation primitive

```rust
/// Process-wide. Monotonic. Written with fetch_max by Reader (focus/mode/nav change)
/// and by the input lane (interrupting key press). Read by everyone, Relaxed/Acquire.
pub struct EpochCell(AtomicU64);
impl EpochCell {
    pub fn advance(&self) -> Epoch;          // fetch_add(1) + 1, Release
    pub fn observe(&self, e: Epoch) -> Epoch; // fetch_max(e), for adopting an epoch carried by a message
    pub fn current(&self) -> Epoch;           // Acquire load
}
```

- **E-1:** Every utterance, query and `PresentJob` carries the epoch it was created in.
- **E-2:** The speech lane checks `utt.epoch >= cell.current()` (or the `survives_focus_change` policy) **at send time** and **at every state transition**. A cancel therefore needs no message: a key press on the input lane advances the cell, and the speech lane, woken by a lightweight `Notify`, cancels on its next poll. This is the fastest path from key press to silence (M-5).
- **E-3:** Results from a worker that arrive with an older epoch are dropped by whoever receives them first. They are never spoken.
- **E-4:** The Reader adopts the epoch carried by an input `KeyIntent` (`observe`), so the utterance answering a key press is never itself cancelled by that key press.

## 4. Core object model

### 4.1 Identity

```rust
pub struct ConnectorId(u16);
/// Unit of isolation. AT-SPI: one bus unique name. Newton (later): one surface.
/// Never reused within a gorca process lifetime.
pub struct AppId { connector: ConnectorId, serial: u32 }
/// Connector-local handle, opaque to core. AT-SPI: FNV-1a hash of the object path (ADR-0004; the
/// bus name is the `AppId`); no generation for path-unique toolkits (I-1).
pub struct LocalId(u64);
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct NodeKey { app: AppId, local: LocalId }
```

Invariants:

- **I-1:** A `NodeKey` never denotes two different objects. If a connector cannot guarantee that an object id is never reused, it bumps a generation on reuse. *(Amended 2026-10-13, B-1 SPEC §15.)* **Removal is not final for a path-unique toolkit:** a successful reply to a request issued after the tombstone revives the key (B-1 LR-3, §4.4); a reply to a request issued before it never does (LR-1). This is safe only because the toolkit never reuses the id for another object: GTK4 paths are `<base>/<uuid>` (`refs/gtk/gtk/a11y/gtkatspicontext.c:1610-1611`) and atk-bridge (GTK3, St/Shell, Firefox, Chromium) numbers objects with a monotonic counter (`refs/at-spi2-core/atk-adaptor/accessible-register.c:155-162`). It is what makes a reorder (`remove` + `add` of the same path) and `RemoveAccessible` for a hidden-but-alive GTK4 context work. For AT-SPI, `LocalId` is therefore the path hash (ADR-0004) without a generation; `RemoveAccessible`, `children-changed::remove` and `defunct` tombstone the key, and an `UnknownObject` reply is authoritative and keeps it dead (atk-bridge leases event-only objects, so a key may die without any event). Browsers may drop hide events (WEB §1.7), so defunct/`UnknownObject` replies are the authoritative fallback. A toolkit that can reuse paths (Qt, out of scope) must bump a generation and its replies never revive; unexpected revivals are counted (`paths_revived_unexpectedly`).
- **I-2:** The core holds no connector type.
- **I-3:** Resolving a `Gone` key yields `Lookup::Gone`. It never panics and never blocks.

### 4.2 Node snapshot

```rust
pub struct NodeSnapshot {
    pub key: NodeKey,
    pub role: Role,                 // core enum: superset mapping of ARIA 1.2 + AT-SPI + AccessKit roles
    pub states: StateSet,           // bitflags (u64), connector-normalized
    pub name: Option<Arc<str>>,
    pub description: Option<Arc<str>>,
    pub value: Option<Value>,       // numeric range or text
    pub parent: Option<NodeKey>,
    pub children: ChildList,        // Known(SmallVec<NodeKey>) | Count(u32) | Unknown
    pub interfaces: Ifaces,         // bitflags: TEXT | EDITABLE_TEXT | HYPERTEXT | TABLE | ACTION | SELECTION | DOCUMENT ...
    pub attrs: AttrMap,             // small, interned keys (xml-roles, level, tag, live-*, ...)
    pub relations: Relations,       // labelled-by, controls, flows-to, ... (keys)
    pub bounds: Option<Rect>,
    /// Fields this snapshot actually carries (B-1). A `Cache.GetItems` node has
    /// NAME|DESCRIPTION|ROLE|STATES|CHILDREN(count)|PARENT; a fetch has what was asked for.
    pub have: PropMask,
    /// Mirror write sequence at which the app confirmed this content: the `issued_at` of the
    /// request whose reply produced it (B-1). Drives the ordering rules of §4.4.
    pub confirmed_seq: u64,
    pub fresh: Freshness,           // see 4.4
}
```

`PropMask` gains `PARENT` (bit 10); `PropMask::PRESENT` is unchanged. A snapshot whose `have` does not cover what a consumer needs is **partial**; it is never padded with defaults.

Text is not in the snapshot. It goes through a `TextQuery` (`GetText(range)`, `CaretOffset`, `Attributes(range)`), because text is large and changes often. For web content the document mirror holds it (§9).

The **interfaces** field is advisory. Browsers advertise `Action`, `EditableText`, `Collection` and `Document` too broadly (WEB §1.2, §2.2). Generators branch on role and states, never on interface presence.

### 4.3 Text units (resolves N-5 and WEB open decision 3)

- **TU-1:** The core API (`TextQuery`, `TextDelta`, caret offsets) uses **Unicode scalar value offsets**.
- **TU-2:** Connectors and sources convert at the edge. Firefox uses UTF-16 units with a phantom U+FEFF after each astral character (`refs/firefox/accessible/atk/DOMtoATK.h:10-28`): strip it and remap. Chromium uses code points. VTE attribute runs use UTF-8 byte offsets.
- **TU-3:** Storage is UTF-8 strings with a sparse scalar-offset index, every 64 scalars. Conversion is O(1) amortised for sequential access and O(log n) for random access.

### 4.4 Freshness and events

*(Amended 2026-10-13, B-1 SPEC §15: representation change; the semantics of the original table are kept, plus the revival rule of I-1.)* A published snapshot is immutable, so the in-flight state no longer lives in it: `Fetching` moved into the fill driver's in-flight table, owned by the app actor.

```rust
pub enum Freshness {
    Fresh { at: Instant },                    // confirmed by connector push or reply
    /// Invalidated by an event; `mask` = fields that are no longer trustworthy.
    /// Still usable for presentation with care (§8.1).
    Stale { since: Instant, mask: PropMask },
    Defunct,                                  // tombstone: removal / app gone; kept briefly
}
/// Driver-side, never published. At most one entry per key (dedupe).
struct InFlight { req: ReqId, issued_at: u64 /* MirrorWriter::seq() */, deadline: Instant }
```

Every mirror write (apply, invalidate, tombstone) advances a per-app monotonic sequence `seq`; every request records `issued_at = seq` when it is issued; invalidations and tombstones record the `seq` at which they happened. Published state × event:

| From \ Event | invalidate(m) at `t` | fetch issued | reply ok to request issued at `s` | reply timeout/err | removed at `t` / `UnknownObject` reply |
|---|---|---|---|---|---|
| Fresh | Stale{m} | unchanged; in-flight entry added (dedupe if present) | Fresh, `confirmed_seq = s` | unchanged (+ health penalty) | Defunct |
| Stale{m₀} | Stale{m₀ ∪ m} | unchanged; in-flight entry added (dedupe if present) | Fresh if no invalidation after `s`; else stored but Stale with the masks invalidated after `s` (**LR-2**, the old "re-dirty") | Stale (+ health penalty) | Defunct |
| Defunct | Defunct | refused, except the proof-of-life fetch (B-1 §5.5) | dropped if `s < t` (**LR-1**); **revived** Fresh if `s > t` (**LR-3**, I-1) | dropped | Defunct |

- **LR-4:** a `CacheDump` applies per node with the same rules. A node absent from a dump is **not** removed by the dump (GTK4 suppresses `AddAccessible` while serving `GetItems`, so a dump may be incomplete); only an event, a cache signal or a failed fetch removes it.
- **Stale epoch:** `invalidate_all` (flood resync, dirty-all, §6.3) is O(1): it records an app-wide epoch, and a read treats every node with `confirmed_seq` below it as `Stale{ALL}`. Reads return `NodeView { snap, stale }` with the epoch folded in.
- **Focus on a `Gone` key is not final:** `FocusChanged` for a tombstoned key issues one Interactive proof-of-life `Fetch` (LR-3) instead of dropping the focus; `UnknownObject` keeps it dead; `BackendError::Unresolved` (path evicted) is a miss, never `Gone`.

Tombstones are evicted after N seconds or when the app becomes `Gone`. A `Stale` or partial node may be presented; the utterance is then tagged `may_refine` and revalidated (§8.1, RV-1). Normative event → mirror effect table, ordering tests and conformance suite: B-1 SPEC §5.

Connectors normalise raw events into a small closed set, and coalescing works on this set:

```rust
pub enum NodeEvent {
    FocusChanged { node: NodeKey },
    ActiveDescendantChanged { container: NodeKey, child: Option<NodeKey> },
    PropertiesChanged { node: NodeKey, mask: PropMask },  // name|description|value|role|states|bounds|attrs
    ChildrenChanged { node: NodeKey },                    // coarse: subtree structure dirty
    TextChanged { node: NodeKey, delta: TextDelta },
    CaretMoved { node: NodeKey, offset: u32, origin: CaretOrigin },
    SelectionChanged { node: NodeKey },
    Announcement { node: Option<NodeKey>, text: Arc<str>, politeness: Politeness },
    WindowEvent { node: NodeKey, kind: WindowKind },      // activate/deactivate/create/destroy
    DocumentEvent { node: NodeKey, kind: DocKind },       // load-start/complete/reload/load-stopped
    NodeRemoved { node: NodeKey },
}
pub enum AppLifecycle { Appeared(AppInfo), Ready, Gone }
```

`AppInfo` carries `toolkit`, `app_name`, `pid` (if known) and `connector_kind`. Extensions match on these; the core never branches on toolkit or app name.

```rust
pub enum TextDelta {
    Insert { offset: u32, len: u32, text: Option<LazyText> },
    Delete { offset: u32, len: u32, text: Option<LazyText> },
    /// Coalesced: a delete+insert pair or > 64 deltas within one drain batch. Consumers re-read.
    Replaced { text: Option<LazyText> },
}
/// Payload is still inside the raw message bytes; decoded only if a consumer asks (F-4).
pub struct LazyText { msg: RawMsg, field: u8 }
```

`CaretOrigin` is `Unknown | ScrollToAnchor`. The connector sets `ScrollToAnchor` for Chromium's `text-caret-moved(0)` on a document (WEB §2.4). Firefox drops granularity and user-origin information (WEB §1.4), so the Reader correlates caret moves with its own recent key intents.

## 5. Connector boundary

### 5.1 Contract

The boundary is message-based, not a callback trait: connectors keep their own concurrency, the core cannot be re-entered, and an out-of-process connector keeps the same contract.

```rust
// crate: gorca-connector-api (depends only on gorca-model, gorca-rt, gorca-ingress)
pub trait Connector: Send + 'static {
    fn kind(&self) -> ConnectorKind;               // "atspi", "newton", "fake"
    /// Runs until shutdown. Must never block the thread; must honour `ctx.shutdown`.
    fn run(self: Box<Self>, ctx: ConnectorCtx) -> BoxFuture<'static, Result<(), ConnectorError>>;
}
pub struct ConnectorCtx {
    pub id: ConnectorId,
    pub registry: AppRegistrar,          // announce/retire apps; returns an AppPort per app
    pub shutdown: CancellationToken,
    pub clock: Clock,                    // virtual in tests
}
/// Given to the connector when it announces an app.
pub struct AppPort {
    pub app: AppId,
    ingress: IngressSender,              // non-blocking, coalescing (6.3); push never awaits
    pub requests: mpsc::Receiver<BackendRequest>,  // core -> connector, bounded
}
impl AppPort {
    pub fn push(&self, ev: RawEvent);                     // O(1), never blocks; may coalesce or mark dirty
    pub fn push_snapshot(&self, nodes: SnapshotBatch);    // bulk cache fill
    pub fn lifecycle(&self, ev: AppLifecycle);
}
pub struct BackendRequest {
    pub id: ReqId,
    pub deadline: Instant,               // mandatory; connector must reply or drop by then
    pub class: BudgetClass,              // Interactive | Prefetch | Background | Startup
    pub op: BackendOp,
    pub reply: oneshot::Sender<Result<BackendReply, BackendError>>,
}
pub enum BackendOp {
    Fetch { node: NodeKey, want: PropMask },
    Children { node: NodeKey, range: Range<u32> },
    Subtree { root: NodeKey, max_nodes: u32 },
    Text(TextQuery),
    Action(ActionReq),                   // DoDefault, Focus, SetCaret, Select, ScrollTo, SetValue
    HitTest { point: Point },
    Collection(MatchRule),
    // Added 2026-10-13 (B-1 SPEC §15). Replies: BackendReply::Subtree(SnapshotBatch); partial
    // nodes carry their `have`.
    /// Children of `node` in `range`, each as a snapshot with at least `want`.
    ChildSnapshots { node: NodeKey, range: Range<u32>, want: PropMask },
    /// `node`, its parent, its parent's parent ... up to `max_depth` (<= 64) or the app root;
    /// stops at a cycle, a foreign reference or a `Document*` role.
    Ancestry { node: NodeKey, max_depth: u8, want: PropMask },
    /// Whole-app bulk dump (AT-SPI: `Cache.GetItems`). `NotSupported` when the app has none.
    CacheDump { max_nodes: u32 },
}
pub enum BackendError {
    Timeout, Gone, NotSupported, Busy, Protocol(String),
    /// The key has no known address (its path was evicted from the connector's path table).
    /// A miss, never `Gone` (B-1 §5.5).
    Unresolved,
}
// SnapshotBatch gains `truncated: bool`: the op hit `max_nodes`, `max_depth` or a size cap.
```

`RawEvent` is a `NodeEvent` whose payload may still be lazy (`LazyText`, R-6); the app actor finishes decoding only for events that survive coalescing.

Connector obligations (conformance suite in `gorca-testkit`):

- **C-1:** `push` never blocks. Per-call cost < 1 µs p99.
- **C-2:** Every `BackendRequest` gets exactly one reply, or the oneshot is dropped, by `deadline + 5 ms`. A dropped sender counts as `Timeout`.
- **C-3:** The connector does its own `Busy` throttling (in-flight limit per app).
- **C-4:** Raw roles and states map to core `Role`/`StateSet` through tables. Unknown values map to `Role::Unknown` plus a raw attribute.
- **C-5:** Each app scope is owned by exactly one connector. The registrar keys apps by `(connector, scope)` and never merges views.
- **C-6:** All app-supplied data is untrusted. Decoding errors and panics are contained in the connector; a malformed message drops that message or that app scope and is counted.

C-5 gains one clause:

- **C-5a:** An app is owned by one connector. A `DocSource` from another transport (CDP, WebExtension) may **attach to a document** of an app owned by the AT-SPI connector (§9). It never owns the app, its focus, or its windows.

### 5.2 Mirror store (PC-1 applied)

```rust
// crate gorca-mirror
/// One per app. Written only by the owning app actor. Read lock-free from any thread.
pub trait MirrorRead: Send + Sync {
    fn get(&self, k: NodeKey) -> Lookup<Arc<NodeSnapshot>>;   // Hit | Miss | Gone
    fn revision(&self) -> u64;                                 // bumped per applied batch
}
pub struct MirrorWriter { /* !Clone, owned by app actor */ }
impl MirrorWriter {
    pub fn apply(&mut self, batch: SnapshotBatch);            // publishes atomically per node
    pub fn tombstone(&mut self, k: NodeKey);
}
```

- **MS-1:** Reads never block and never allocate on a hit.
- **MS-2:** Per-node consistency only. Cross-node consistency, such as parent and child agreeing, is not guaranteed. Code that needs it uses a document snapshot (§9) or re-validates with a query.
- **MS-3:** Implementation candidates are a lock-free hash map (`papaya`, `scc`), `left-right`, or a sharded map of `ArcSwap`s. This is decided by microbenchmark MB-1, not a bake-off.

### 5.3 AT-SPI transport: drains, call connections, deadlines (F-3)

```

 a11y bus ─── event connection (bus) ── drain-0 ──────────────► per-app ingress (ring + coalescer)
              (match rules: all subscribed events + NameOwnerChanged)

 a11y bus ─── call connection (bus) ──── workers ─── registry calls, fallback calls
 app P2P  ─── call connection per app (GetApplicationBusAddress) ── workers ─── per-app requests
```

- **D-1 Drains never wait downstream.** A drain reads in large batches (≥ 64 KiB per `recv`) and frames messages. It peeks only the header fields (sender, path, interface, member, signature, serial). It computes the coalescing key and calls `ingress.push`, which is O(1) and never awaits (C-1). If an app's ingress is full, the event becomes a dirty mark (R-3). The drain does not deserialize bodies.
- **D-2 Lag detection.** dbus-broker exposes no statistics, so the drain samples its own socket backlog with `ioctl(SIOCINQ)` every batch and exports it. Backlog over a watermark (default 4 MiB) triggers a global `Shed` signal: app actors move their noisiest non-focus apps to `Flooded`, and per-app event deregistration (D-5) is applied. Broker-side disconnection must never happen (M-11).
- **D-3 Replies never share a socket with the flood.** Method calls go over P2P connections per app (WEB §1.6: -35 to -60 % p50) or over a separate bus call connection. Event connections never carry method calls except `AddMatch`/`RemoveMatch`, which are sent with a deadline.
- **D-4 Deadline helper.** One `call(deadline, ...)` helper wraps `tokio::time::timeout`. Direct zbus proxy calls are banned with `clippy::disallowed_methods`. The connection-wide `method_timeout` (2 s) is a backstop.
- **D-5 Inflow reduction at the source.** Subscribe to the minimal event set through `org.a11y.atspi.Registry.RegisterEvent` (apps filter unregistered events in-process, LP §2), and install narrow match rules ourselves. atspi-rs `remove_match_rule` actually adds a rule (`refs/atspi-rs/atspi-connection/src/lib.rs:262`), so we do not use it. *Proposed:* when a non-focused terminal floods, deregister `text-changed` for that app with the `app` argument of `RegisterEvent`, and re-register on focus. Whether atk-bridge honours per-app registration is open question P-11. Note that ATK still computes event text before filtering (LP §2), so this saves broker and drain cost, not app cost.
- **D-6 Own connections.** We build our own zbus connections with `max_queued` ≥ 4096 (zbus default is 64, `DEFAULT_MAX_QUEUED`). We do not use `atspi::AccessibilityConnection`, which cannot be configured and whose `p2p` startup calls every app in sequence with no timeout (`refs/atspi-rs/atspi-connection/src/p2p.rs:304-337`). The event drain does not use zbus at all: it is our own raw framing reader (B-2 → ADR-0004 D-2); zbus is used for call connections only.
- **D-7 P2P lifetime.** A P2P connection is opened lazily on the first request, closed when the app's bus name vanishes, and treated as `Gone` on EOF. Opening it has a `Startup`-class deadline. If it fails, requests go over the bus call connection.

### 5.4 AT-SPI cache fill

- Native apps (GTK3/4, Shell, terminal): `Cache.GetItems` (4–5 ms for ~300 items, F-6) is an arm of bake-off B-1.
- Web documents: `GetItems` does not return document nodes (F-6). Document content goes through the document mirror and its `SourceLayer` (§9).

### 5.5 Platform identity and enablement (R1, F-1, F-8, F-9)

- **ID-1 Bus name.** At startup gorca calls `RequestName("org.gnome.Orca.KeyboardMonitor", flags = 0)` (queued) with a deadline. Reply `PRIMARY_OWNER` → input monitor `Authorizing`. Reply `IN_QUEUE` → `Unauthorized { queued: true }`: gorca speaks once ("Another screen reader holds the keyboard; gorca will take over when it exits") and runs without its own key commands. `NameAcquired` moves it on (§7.3). Development setups may use `MUTTER_DEBUG_A11Y_MANAGER_WITHOUT_ACCESS_CONTROL=1`.
- **ID-2 Process name.** The main thread sets `comm` to `orca` with `prctl(PR_SET_NAME)` **before** setting `ScreenReaderEnabled`. Chromium's `IsOrcaProcess` reads the thread-group leader's `comm` (`browser_accessibility_state_impl_auralinux.cc:43-56`). Other threads are named `gorca-<lane>`.
- **ID-3 Side effects of ID-2 that must be handled:**
  - Orca's `other_orcas()` runs `pgrep -u $UID -x orca` (`refs/orca/src/orca/orca_bin.py.in:208-219`). Orca will therefore refuse to start while gorca runs.
  - `orca --replace` sends **SIGKILL** to gorca (`orca_bin.py.in:342-343, 221-238`). The systemd user unit sets `RestartPreventExitStatus=SIGKILL` so gorca does not fight Orca in a restart loop.
  - `orca --setup` sends **SIGUSR1** to gorca, whose default action would terminate the process. gorca installs a SIGUSR1 handler: it opens gorca settings or is ignored.
- **ID-4 Chromium.** The documented launcher adds `--force-renderer-accessibility` as a fallback for builds or forks that do not run `/proc` detection.
- **ID-5 Enablement.** At startup gorca sets `org.a11y.Status.IsEnabled = true` and `ScreenReaderEnabled = true`. It does not toggle them off while running. Shutdown restores the previous values.
- Upstream fixes, such as a second allow-listed name in Mutter or a generic AT detection in Chromium, are a later nice-to-have and do not block anything.

### 5.6 Toolkit normalisation and the terminal case (F-4, F-8)

Toolkit quirks that more than one consumer needs live in the connector's normalisation tables (rule in §10: a workaround needed by two consumers moves down a layer). They are keyed by `AppInfo.toolkit`, never by ad-hoc code:

| Quirk | Normalisation |
|---|---|
| Firefox UTF-16 + U+FEFF offsets | TU-2 conversion |
| Firefox lazy cache domains | One warm-up query set per document, off the hot path; "empty text on a node with children" is treated as `Stale`, not as a value (WEB §1.5) |
| Chromium `text-caret-moved(0)` on document | `CaretOrigin::ScrollToAnchor` |
| Chromium no text events for static text | `children-changed` on a parent marks its subtree text dirty |
| Firefox dropped hide events | I-1: `defunct`/`UnknownObject` replies tombstone the key |
| VTE attribute runs in UTF-8 bytes | TU-2 |
| VTE `CharacterCount` ≠ `GetText` length | Never use `CharacterCount`; length comes from the last `GetText` |

**Terminal rules (F-4):**

- **T-1 Collapse per frame.** In the coalescer, a `Delete` followed by an `Insert` on the same node within one drain batch becomes `Replaced`. Under a flood, every frame collapses to a single slot per terminal. The payload stays in the raw message (`LazyText`) and is decoded only for the surviving slot.
- **T-2 Diff the screen ourselves.** The terminal presenter keeps the previous screen snapshot per terminal and computes new output with a line-hash diff on `cpu`, in the app actor's job, never on the Reader. `ext-vte` supplies policy: prompt detection, what counts as "new output", and review integration.
- **T-3 Never crash the terminal.** Every text request to an app clamps offsets to `[0, last_known_len]` and never sends negative values. VTE's `g_assert(start_offset >= 0)` would abort the whole terminal server.
- **T-4 Background terminals.** A non-focused terminal under flood goes to `Flooded` quickly. Its only cost is a dirty bit, plus D-5 deregistration if P-11 confirms that it works.

### 5.7 Newton (R5)

Newton is not a runtime dependency. The connector API (`push`, `push_snapshot`, local query answering) already covers a push-model source. The Newton analysis is in the prior-art report §3; deferred questions N-1…N-4 are listed in §16.3. The `gorca-connector-newton` crate is **not scaffolded** until Newton has app identity and access control. Key handling depends on `gorca-input-api::KeyEvent`, never on a D-Bus interface name, so a Newton `KeyEvent` source can be added behind the same trait.

## 6. Actor topology

```
                  hot lanes (dedicated threads)                     workers (tokio MT) + cpu (rayon)
 Mutter KeyEvent ─► [input] ─KeyIntent─►┐
                      │ EpochCell.advance()                    ┌──────────────────────────────┐
 a11y bus ─► [drain-0] ────push─► per-app ingress ───────────► │ App actor (1/app)             │
                                                               │  mirror shard (writer)        │
                                                               │  health SM, request mux       │
                                                               │  └─ Doc actors (1/web doc)    │
                                                               │       doc mirror + SourceLayer│
                                                               └──────┬─────────────▲─────────┘
                          lock-free reads of mirrors/doc snapshots    │ events      │ queries
                     ┌────────────────────────────────────────────────▼─────────────┴─┐
                     │ [reader] Reader actor: FocusSM, ModeSM, nav cursors, inline     │
                     │ presenter for hot path; PresentJob → cpu for bulk               │
                     └───────┬───────────────────────────────────────────────────────┘
                             │ SpeechIntent (epoch)       ▲ PresentJob results (epoch) go here directly
                             ▼                            │
                     [speech-io] SpeechQueue SM ── SpeechClient ──► voicego (native) | speech-dispatcher (SSIP)
                     Braille actor (workers) ── BrlAPI
                     Extension host (workers): one wasmtime Store per (extension, app)
```

### 6.1 Actors and ownership

| Actor | Count | Lane | Owns | Never does |
|---|---|---|---|---|
| Supervisor | 1 | workers | spawn and restart policy, telemetry | a11y logic |
| Input | 1 | input | keyboard-monitor connection, grab-set SM, keymap, modifier state | IPC to apps |
| Drain | 1 | drain | event connection socket | body decoding, awaits on downstream |
| Connector request tasks | per app | workers | P2P call connection, interners, raw→core mapping | presentation |
| **App actor** | 1 per app | workers | mirror writer, coalescer drain, `AppHealth` SM, request mux, bound extension instances, child doc actors | user-visible state |
| **Doc actor** | 1 per web document | workers (+cpu) | document mirror, indexes, flat text, live-region classifier, `SourceLayer` attachment | user-visible state |
| **Reader** | 1 | reader | Focus SM, Mode SM, nav cursors, hot-path presentation | awaiting anything |
| Speech | 1 | speech-io | `SpeechQueue` SM, `SpeechClient` | presentation |
| Braille | 1 | workers | regions, panning, BrlAPI | presentation |
| Extension host | pool | workers | wasmtime engine, stores | direct IPC |

### 6.2 Channels and backpressure

| Edge | Channel | Bound | When full |
|---|---|---|---|
| drain → app ingress | lock-free ring (1,024) + coalescing table (§6.3) | 4,096 distinct keys per app (`Flooded`); 65,536 hard cap (rule H); global `SlotBudget` + per-app reserve (IB-6, IB-7) | ring full: producer folds a bounded chunk; `Flooded`: dirty marks, no queueing; hard cap / budget: dirty-all or preemption of the largest holder. Never waits. |
| app actor → doc actor | bounded mpsc of event batches | 64 batches | merge into the doc's dirty set |
| app/doc actor → Reader | bounded mpsc, focus lane + background lane | 256 / 64 | background: drop oldest non-focus event, set `resync_needed`. Focus lane: coalesced in the app actor; `FocusChanged` is never dropped |
| Reader → app/doc actor (queries) | bounded mpsc | 64 | synthesize `Busy`; the Reader falls back to the snapshot |
| app actor → connector | bounded mpsc | 32 in flight per app | coalesce identical `Fetch`; reject `Background` class first |
| Input → Reader | bounded mpsc, priority lane | 64 | overflow is a bug; logged |
| Reader / cpu → Speech | single-slot latest-interrupt + FIFO | 1 + 32 | interrupting intent replaces the slot; FIFO drops lowest-priority-oldest |
| Speech → `SpeechClient` writer | SPSC | 4 commands | a newer `Replace` supersedes queued commands; see §8.3 |

The Reader polls with a **biased select**: input, focus lane, query replies, background lane.

### 6.3 Ingress coalescing

The per-app ingress is a keyed, order-preserving structure written by the drain and drained by the app actor. The implementation was decided by B-2 → **ADR-0004** (raw-socket drain, bounded lock-free ring per app, fold in ring order on the consumer or, in bounded chunks, on a producer). The rules and invariants below are architectural: any future implementation must keep them, and each one has a test in `crates/gorca-ingress/tests/` or `crates/gorca-bench-b2/tests/`. The executable model `crates/gorca-bench-b2/src/model.rs` is normative for single-producer output (B-2 SPEC §4.4).

```rust
enum CoalesceKey {
    Focus,                     // last-writer-wins, one slot per app
    Props(NodeKey),            // OR the PropMask; value fetched lazily
    Children(NodeKey),         // collapse; ancestor ChildrenChanged subsumes descendants (depth-limited check, TBD)
    Text(NodeKey),             // TextDelta merge (R-TEXT)
    Caret(NodeKey),            // last-writer-wins
    Selection(NodeKey),
    ActiveDescendant(NodeKey), // last-writer-wins per container (added by B-2)
    Announcement(Seq),         // never coalesced, capped per drain
    Window(NodeKey), Document(NodeKey), Removed(NodeKey),
}
```

**Coalescing rules**

- **R-1 Supersession.** `Removed(n)` deletes every pending slot for `n` (including an older `Removed(n)`) and then takes its own slot; later events for `n` get new slots after it. `Focus` keeps only the newest node; the sequence length is kept for statistics.
- **R-2 Ordering.** Order-preserving by first occurrence; later events for the same key update the slot in place (last-writer-wins for `Focus`, `Children`, `Caret`, `Selection`, `ActiveDescendant`, `Window`, `Document`, `Removed`; OR for `Props`). Per-app causal order per node (ORD-1: payload counters per node never go backwards across batches); no cross-app ordering. Several producers on one ingress keep each producer's program order.
- **R-3 Overload (`Flooded`).** When a new key would exceed `max_keys` (4,096), or the app actor lags more than 50 ms, the app enters `Flooded`: every pending slot of a non-kept class becomes a dirty mark of its node, in slot order, and later events of those classes become dirty marks (dirty-all when they have no node). **Kept classes** queue normally: `Focus`, `Removed`, `Window`, announcements (within the cap), and `Caret(n)` when `n` is the node of the latest focus. `Flooded` ends with the drain; `Drained::dirty` is `Some` exactly when the batch was flooded. The app actor resyncs lazily: nodes the Reader asks for, plus focus ancestry.
- **R-H Hard cap (rule H).** A push of a new key other than `Focus` with `live ≥ hard_cap` (65,536) enters `Flooded` if not yet, sets the dirty set to "everything" (an announcement is counted in `announcements_dropped` instead) and queues nothing. `Focus` is one slot and always queues, so pending slots never exceed `hard_cap + 1`. After dirty-all, the batch may lack removals (the resync rediscovers them) but never invents any.
- **R-4 Cost.** Push is O(1), lock-free and allocation-free in steady state (one CAS on the ring, one on the budget). Targets: p99 ≤ 1 µs single producer, ≤ 5 µs with four producers (B-2 T7); 100k pushes/s per app on one core.
- **R-TEXT / R-5.** Text deltas append in order. A `Delete` followed by an `Insert`, more than `max_text_deltas` (64) deltas, or an existing `Replaced` collapse the slot to one `Replaced { text: None }` (T-1, all toolkits). Adjacent-insert concatenation and insert/delete cancellation are deferred to the decoded-text step (R-6); an implementation that adds them must leave every other behaviour identical.
- **R-6 Lazy bodies.** Slots hold raw message handles, not decoded bodies (`LazyText`, lazy property values), so superseded events cost no decoding.

**Resource bounds** (B-2 SPEC §3.6; every bound degrades to resync, never to unbounded growth or a silently lost lossless event)

| ID | Bound | Value | Beyond it |
|---|---|---|---|
| IB-1 | Pending slots per app before `Flooded` (`max_keys`) | 4,096 | R-3 |
| IB-2 | Pending slots per app, lossless classes included (`hard_cap`, R-H); a text slot holding k deltas counts k (AU-4) | 65,536 (+1 `Focus`) | R-H |
| IB-3 | Records in the per-app ring (`RING_CAP`) | 1,024 | the pushing producer folds one bounded chunk (≤ 128) into the table |
| IB-4 | Dirty set nodes | 65,536 | the set becomes "everything" |
| IB-5 | Announcements per drain / announcement text | 16 / 4,096 bytes (clipped at a char boundary in the mapping) | dropped and counted in `announcements_dropped` |
| IB-6 | Pending slots across all apps: one shared `SlotBudget` pool (`GLOBAL_SLOT_BUDGET`), plus a flat reserve **R = 32** per app (`IngressConfig` default) outside the pool | global bound `GLOBAL_SLOT_BUDGET + apps × (R + 1)` = 262,144 + 33 per app | IB-7 |
| IB-8 | Superseded-slot tombstones | ≤ 2 × live + 256 | incremental compaction |
| IB-9 | Cached sender routes per event connection (`MAX_ROUTES`) | 4,096 | cache dropped and rebuilt; disconnected senders retired on `NameOwnerChanged` |
| IB-10 | Bytes of one incoming message held in memory | 16 MiB | skipped while streaming, counted `malformed` |
| IB-11 | Memory held by an idle, drained ingress | ≤ 1 MiB | capacity above it freed on drain (AU-5) |

**IB-7 Budget, reserve and preemption.** What a slot charge does when it needs one:

```rust
enum Charge {
    Reserve,              // app holds < R: always granted, from its reserve (never from the pool)
    Pool,                 // CAS on SlotBudget::used that never exceeds the limit (AU-7)
    Preempt { holder: AppId }, // pool exhausted: largest holder above its own reserve, holding more than the requester
    Refuse,               // no eligible holder: the requester applies R-H to itself
}
```

| Requester state | Pool | Largest other holder `h` | Result |
|---|---|---|---|
| holds < R | any | any | `Reserve` |
| holds ≥ R | not exhausted | any | `Pool` |
| holds ≥ R | exhausted | `h.held > R` and `h.held > requester.held` | `Preempt`: `h` is flooded and set dirty-all, all of `h`'s charges above its reserve return to the pool, the requester retries `Pool` once |
| holds ≥ R | exhausted | otherwise | `Refuse` → R-H on the requester |
| `Focus` (any) | any | any | always queues (R-H); charged from the reserve or the pool, never refused |

- **IB-7a** Preemption never takes a holder below its reserve and never preempts the requester itself. There is no other minimum holder size (B-2's 128-slot threshold was the AU-3 defect, ADR-0004 BF-1).
- **IB-7b** Preemption is O(1) under the holder's lock: its table is swapped out and handed to the reaper thread (`gorca-reaper`). If the reaper's bounded channel is full, the table rides on the holder's next `Drained` and is freed by its app actor on `workers`, never on the drain thread.
- **IB-7c** Fairness is per connection (`AppId` = sender). Per-process fairness is future work and must not add a method call to the drain (D-3).

**Audit invariants** (B-2 SPEC §3.7; binding on every implementation, tests `crates/gorca-ingress/tests/audit_*.rs`)

| ID | Invariant |
|---|---|
| **AU-1** | Charges are exact. At every observable point, Σ over live ingresses of (pending slots + extra text deltas + ring records) == `SlotBudget::used` (checked as equality, never `≥`). Dropping an ingress or its receiver returns every charge. |
| **AU-2** | A push into an ingress whose receiver is gone charges nothing. A sender that disconnects loses its cached route in the drain. |
| **AU-3** | An app holding at most R pending slots never loses an event because of another app's overflow; an app above R can lose events to another app only by being preempted (flooded + dirty-all, so it resyncs), and only if it is the largest holder. No event is ever lost without a dirty mark or a counter. |
| **AU-4** | The budget and `hard_cap` count queued events including text deltas (a text slot with k deltas is charged k). |
| **AU-5** | An idle, drained ingress holds ≤ 1 MiB (IB-11). |
| **AU-6** | No single push, fold chunk, drain chunk or preemption holds a lock for more than 250 µs worst case at `hard_cap`. Amortised O(1) is not sufficient: compaction, drain and frees are incremental or deferred to the reaper. |
| **AU-7** | Concurrent charges never overshoot the pool, `Focus` included. |
| **AU-8** | Counters equal structure sizes after every push, merge, supersede, flood, drop, drain and preemption (`IngressReceiver::check_invariants`, `debug_assert` in debug builds); no `saturating_sub` on a counter that must be exact. |

The budget is created once by `gorca-daemon` and passed explicitly to the router / app-channel factory; there is no process-wide singleton (ADR-0004 D-6).

### 6.4 Deadlines and app health

| Budget class | Default deadline | Used for |
|---|---|---|
| Interactive | 120 ms total per user action, split across calls | focus presentation, nav step, key-driven queries |
| Prefetch | 300 ms | ancestry, siblings, document chunks |
| Background | 2 s | full subtree sync, `GetItems` |
| Startup | 5 s | first contact with a new app, P2P connect |

The 15 ms goal excludes IPC wait, but the Interactive deadline bounds what the user hears: on a miss, the Reader speaks from the cache and marks the result `may_refine`.

```rust
enum AppHealth {
    Healthy,
    Slow    { timeouts: u8, window_start: Instant },     // >= 3 timeouts / 2 s
    Hung    { since: Instant, probe_at: Instant },       // >= 3 consecutive Interactive timeouts
    Flooded { since: Instant, dirty: DirtySet },
    Gone,
}
```

| State | Event | Next | Effect |
|---|---|---|---|
| Healthy | timeout | Slow | halve Prefetch concurrency |
| Slow | 3 consecutive timeouts | Hung | reject Prefetch/Background; Interactive from cache only; announce "application not responding" once if focused |
| Slow | 10 s without timeout | Healthy | restore concurrency |
| Hung | probe tick (backoff 250 ms → 4 s) | Hung / Healthy | one cheap probe (`GetRole` on app root), Interactive deadline |
| Healthy, Slow | ingress over watermark | Flooded | R-3 |
| Healthy, Slow | global `Shed` (D-2), app not focused, top-3 by ingress rate | Flooded | D-5 deregistration if supported |
| Flooded | backlog drained | Healthy | lazy resync of the dirty set; re-register events |
| any | owner vanished / connector `Gone` | Gone | tombstone all nodes; Reader gets `AppLifecycle::Gone` |

**I-4:** A `Hung` app receives no request except a probe, and at most one probe at a time.

### 6.5 Failure isolation

- Every actor is a supervised task or lane. A panic is caught at the task boundary (`JoinError::is_panic`, or `catch_unwind` on a lane thread) and the actor restarts with fresh state from the connector (resync). Release builds keep `panic = "unwind"` for this.
- **App actor panic:** only that app actor restarts. Focus may be lost; the Reader re-queries the focused node.
- **Untrusted decoding** (atspi `ObjectRef` asserts, malformed bodies) is contained per message or app scope (C-6). A crash loop quarantines that scope; the user hears "application not accessible" once.
- **Reader panic:** restarted with state machines reset to `NoFocus`, and "gorca restarted" is spoken. The Reader has no IPC and no extension code, so its panic surface is small.
- **Doc actor panic:** only that document is rebuilt. The caret is restored from its bookmark (§9.4, IR-7).
- **Speech service crash:** this is a separate process now. The speech SM goes to `Disconnected` (§8.2), and gorca itself is unaffected. Braille, if present, continues.
- **Drain panic:** the drain owns no state beyond the socket. The supervisor reconnects, and every app goes through a full resync (dirty-all). Ingress continuity is lost, by design.

### 6.6 Watchdog

An OS thread outside tokio watches the heartbeats of input, drains, reader and speech-io: the time since the last processed item while the queue is non-empty. A stall over 250 ms is logged with a backtrace. A stall over 2 s triggers a supervised restart. Drain stalls also trip on `SIOCINQ` growth with no progress.

## 7. State machines

All state machines are `enum`s with exhaustive `match` and no wildcard arm. Each is unit-tested as a pure `fn step(self, Msg) -> (Self, Effects)`.

### 7.1 Focus

```rust
enum FocusState {
    NoFocus,
    Settling { candidate: NodeKey, epoch: Epoch, until: Instant },   // debounce flapping focus
    Focused  { node: NodeKey, epoch: Epoch, presented: bool },
    Orphaned { last: NodeKey, epoch: Epoch },                       // focused node removed / app gone
}
```

| From | Msg | To | Effects |
|---|---|---|---|
| NoFocus / Focused / Orphaned | `FocusChanged(n)` | Settling{n, `EpochCell.advance()`, now+δ} | stale speech cancels immediately through E-2, not after δ |
| Settling{c} | `FocusChanged(n)` | Settling{n, advance, now+δ} | — |
| Settling{c} | timer δ | Focused{c, presented: false} | mirror read (lock-free); on miss, `Fetch(c, PRESENT_MASK)` + ancestry (Interactive) |
| Focused{n, !presented} | cache hit or reply | Focused{n, presented: true} | `SpeechIntent(Interrupt, epoch)` |
| Focused{n} | `NodeRemoved(n)` / app Gone | Orphaned | query the active window's focus (Interactive) |
| Orphaned | `FocusChanged(n)` | Settling | — |
| any | focus app goes `Hung` | unchanged | speak "not responding" once per Hung episode |

δ is 0 ms by default and 30 ms for roles known to flap (menu, menu item, popover); extensions can tune it (M-6). The epoch also advances on every mode change and explicit navigation.

### 7.2 Mode

```rust
enum Mode {
    Focus,                                                      // follow system focus; keys pass through
    Browse  { doc: DocId, cursor: DocCursor, sticky: bool },    // virtual cursor over the document mirror
    Review  { origin: NodeKey, cursor: ReviewCursor },          // flat/object review; terminal review
    PassThrough { scope: PassScope },                           // OneKey | UntilToggle
    InputHelp,                                                  // learn mode: keys described, not executed
}
```

Transitions come from user commands, from focus entering or leaving a document, and from editable focus inside a document (NVDA-style auto focus mode). Extensions may veto or force auto-switching. Every transition emits `SetGrabSet(mode.grab_set())` (§7.3); `InputHelp` uses `GrabKeyboard`. The full table goes in `docs/design/modes.md` (open item).

### 7.3 Keyboard monitor and grab set (F-1, F-2)

```rust
enum MonitorState {
    Unauthorized { queued: bool },                 // name owned elsewhere (Orca)
    Authorizing  { deadline: Instant },            // WatchKeyboard in flight
    Watching     { grabs: GrabSetState },
    Lost         { retry_at: Instant },            // Manager vanished (shell restart) or name lost
}
enum GrabSetState {
    Committed { gen: u32, set: GrabSet },
    Pending   { committed: (u32, GrabSet), wanted: (u32, GrabSet), sent_at: Instant },
}
enum KeyClass { Command(CmdId), PassToApp, Ambiguous, Injected }
```

| From | Msg | To | Effects |
|---|---|---|---|
| Unauthorized | `NameAcquired` | Authorizing | `WatchKeyboard` (deadline 500 ms) |
| Authorizing | reply ok | Watching{Committed{0, ∅}} | `SetGrabs(mode set)` |
| Authorizing | timeout / `AccessDenied` | Lost{backoff} | speak once |
| Watching{Committed g} | `SetGrabSet(s)` with s ≠ g.set | Watching{Pending{g, (g+1, s)}} | async `SetKeyGrabs` (deadline 200 ms) |
| Watching{Pending} | `SetGrabSet(s2)` | Pending{committed, (gen+1, s2)} | send again; the latest wins |
| Watching{Pending} | reply ok, then settle of 5 ms (Mutter applies modifier grabs asynchronously on its input thread) | Committed{wanted} | — |
| Watching{Pending} | reply timeout | Pending (retry ×2) then Lost | log |
| any | `NameLost` / Manager `NameOwnerChanged` to empty | Lost{retry_at} | the Reader announces "keyboard commands unavailable" |
| Lost | timer | Authorizing | — |

**Key classification** on every `KeyEvent` (input lane, O(1)):

- A key in the committed grab set is a `Command`. Mutter swallowed it.
- A key in neither set passes to the app. It is observed for echo and interrupt only.
- A key in `committed Δ wanted` during `Pending` is `Ambiguous`. It is classified by the **committed** set, because that is what Mutter most likely applied. It is counted (M-16), never re-executed and never re-injected.
- A key matching the injection-expectation table is `Injected` and ignored (self-feedback suppression).

Invariants:

- **K-1:** Grab sets are minimal. gorca grabs only its modifier, explicit command chords, and, in Browse mode, the quick-navigation keys. Everything else is watched, never grabbed. **Prefer changing the grab set over re-injection.**
- **K-2:** Re-injection (`KeyInjector`, RemoteDesktop or EIS session) is used only for keys that were swallowed but turn out to be meant for the app. Examples: a quick-nav letter typed while an editable field takes focus inside the 5 ms settle window, and pass-through of a single key that is in the grab set. Injected keys go into an expectation table `(keycode, pressed) → deadline 50 ms`. Virtual-device repeats are dropped by Mutter (`update_button_count`), so held-key repeat is not re-injected.
- **K-3:** Interrupting keys (policy: any non-modifier press, configurable) advance the `EpochCell` **on the input lane**, before the Reader sees the key. This gives silence-on-keypress without a Reader hop.
- **K-4:** Every keyboard-monitor call is async with a deadline. Orca and libatspi use sync calls with no deadline (`refs/at-spi2-core/atspi/atspi-device-a11y-manager.c`).
- **K-5:** The monitor uses its own session-bus connection with one match rule, drained on the input lane. The F-3 disconnect rule applies on the session bus too.
- **K-6:** `SetKeyGrabs` matches modifier masks exactly, so each chord is registered with its NumLock and CapsLock variants (×4), as libatspi does.
- **K-7:** Shortcut-inhibiting windows (VM viewers, remote desktop) deliver no key events. The Reader announces this once per window activation if the active window is known to inhibit, and the gorca modifier is unavailable there.

### 7.4 Speech queue (R4)

```rust
enum SpeechState {
    Idle,
    Speaking  { cur: UttId, prio: Prio, epoch: Epoch, sent_at: Instant, started: bool },
    Replacing { from: UttId, to: UttId, deadline: Instant },   // atomic replace or stop+speak in flight
    Paused    { resume: Option<UttId> },
    Disconnected { retry_at: Instant, attempt: u32 },
}
enum Prio { Interrupt, Message, Text, Progress, Notification }
```

| From | Msg | To | Effects |
|---|---|---|---|
| Idle | intent u (valid by E-2) | Speaking{u} | `Speak(u)` |
| Speaking | intent u with prio Interrupt or newer epoch | Replacing{cur→u} | `Replace{stop: cur, then: u}` if `caps.atomic_replace`, else `Stop(cur)` then `Speak(u)` pipelined without waiting for the ack |
| Speaking | `EpochCell` advanced past cur.epoch | Idle or Replacing | `Stop(cur)`; drop stale FIFO entries |
| Speaking | `Ended(cur)` | Idle or Speaking{next} | pop the FIFO and re-check E-2 |
| Replacing | `Started(to)` or `Ended(from)` | Speaking{to} | — |
| Replacing | deadline (30 ms) | Speaking{to} | count `replace_slow`; health penalty |
| any | transport EOF or error | Disconnected{backoff} | intents older than 2 s are discarded on reconnect |
| Disconnected | reconnect ok | Idle | re-send voice params |

- **S-1 gorca is the queue authority.** The server holds at most `caps.max_in_flight` utterances: 1 for SSIP, and up to 2 for voicego if it supports targeted cancel by id (so say-all has no gap). All other queued speech lives in gorca.
- **S-2** The validity check (E-2) runs at send time and on every transition.
- **S-3** No speech command ever blocks the speech lane. The `SpeechClient` writer is a separate task with a 4-slot SPSC queue. If the server stops reading, newer `Replace` commands supersede queued ones, and after the deadline the client reports `Degraded`.

## 8. Output pipeline and the speech service (R4 changed)

### 8.1 Pipeline

```
Reader (hot path) / cpu PresentJob ─► Utterance{segments, lang, epoch, prio, may_refine}
  ─► verbalize (punctuation, symbols, pronunciation dict; on producer thread or cpu)
  ─► Speech lane: SpeechQueue SM ─► SpeechClient ─► separate speech service process
  ─► Braille actor (same segments, same epoch)
```

*(Amended 2026-10-13, B-1 SPEC §15.)* **Every presentation served from the mirror is tagged `may_refine`**, not only `Stale` ones: GTK4 and St names change silently (no `accessible-name` event for content- or `labelled-by`-derived names), and replies and events travel on different connections with no mutual order, so even a `Fresh` hit can be wrong. Rules:

- **Present or fetch.** A stale or partial read is presented from the mirror only if the missing or stale fields are not NAME or ROLE; otherwise the Reader fetches (Interactive) and presents the reply.
- **RV-1 revalidation.** Every cache-served presentation (focus, state change of the focused node, where-am-I) is followed by exactly one `Fetch{node, PRESENT}` with class Interactive, unless the presented snapshot was confirmed by a request issued after the triggering event. If the reply differs in a presented field, the Reader refines (rule below) or, past the threshold, speaks a short follow-up with the changed field. Both count as corrections for M-6.

The `may_refine` rule: a refined utterance replaces the original only if the epoch is unchanged and less than 150 ms of audio has been played (threshold validated by M-6). The Speech lane needs `Started` and audio-progress events from the backend to apply it. If the backend has no audio-progress events, refinement is limited to utterances that have not yet been `Started`.

### 8.2 `SpeechClient` abstraction

```rust
// crate gorca-output-api
pub trait SpeechClient: Send + 'static {
    fn caps(&self) -> &SpeechCaps;
    /// Never blocks. Ok = queued to the writer task, not "spoken".
    fn submit(&self, cmd: SpeechCmd) -> Result<(), SubmitError>;   // Full | Disconnected
    /// Server events, drained by the speech lane. Bounded; overflow coalesces progress events.
    fn events(&mut self) -> &mut mpsc::Receiver<SpeechEvent>;
    fn health(&self) -> ClientHealth;                                 // Connecting | Ready | Degraded | Down
}
pub enum SpeechCmd {
    Speak   { id: UttId, text: Verbalized, voice: VoiceRef, marks: bool },
    Replace { stop: UttId, then: Box<SpeechCmd> },   // atomic if caps.atomic_replace, else lowered by the client
    Stop    { id: UttId },                            // stop this utterance only
    StopAll,
    Pause, Resume,
    SetParams(VoiceParams),
}
pub enum SpeechEvent { Accepted(UttId), Started(UttId), Mark(UttId, u32), AudioProgress(UttId, Duration), Ended(UttId), Stopped(UttId), Error(UttId, SpeechErr) }
pub struct SpeechCaps {
    pub atomic_replace: bool,     // one command = stop + speak (no ack wait)
    pub utterance_ids: bool,      // events and Stop are addressed by gorca's UttId
    pub max_in_flight: u8,        // server-side queue depth gorca may use (S-1)
    pub index_marks: bool,
    pub audio_progress: bool,     // played-duration or first-PCM events
    pub ssml: bool,
}
```

| Backend | Crate | Role | Wire |
|---|---|---|---|
| voicego native | `gorca-speech-voicego` | **primary** | **TBD-V1…V6** (below). go-engineer is auditing `refs/voicego` in parallel. |
| SSIP | `gorca-speech-ssip` | compatibility: speech-dispatcher, and voicego in text-SSIP mode | text SSIP. `Speak` = `SET self PRIORITY text` + `SPEAK`; `Stop` = `STOP self`; `Replace` lowered to `STOP self` + `SPEAK`, pipelined. `utterance_ids` are emulated by mapping SSIP msg-ids from `225 OK MESSAGE QUEUED` replies. `max_in_flight = 1`. Events via `SET self NOTIFICATION all on` (`docs/platform/ssip-speech-dispatcher.md`). |

**TBD hooks for voicego** (filled after the go-engineer audit; the trait above must not change for them):

- **TBD-V1 Wire framing.** Is the native protocol voicego's binary SSIP framing (`refs/voicego/BINARY_SSIP.md`), a separate protocol, or text SSIP with extensions?
- **TBD-V2 Atomic replace.** Does a single command stop the current utterance and start a new one with no round trip? If not, `atomic_replace = false`.
- **TBD-V3 Ids and events.** Can events and stop commands be addressed by a client-chosen id? Are `Started`, `Mark` and `Ended` delivered in order per id?
- **TBD-V4 Audio progress.** Is there an event for "first PCM handed to the sink" (true audio start) and for played duration? This is needed for `may_refine` and M-12.
- **TBD-V5 Transport and auth.** Unix socket path, discovery, and whether mTLS or other options apply locally.
- **TBD-V6 Failover.** *Proposed (needs sign-off):* if voicego is unreachable at startup or after 3 failed reconnects, fall back automatically to the SSIP backend (speech-dispatcher), announce it once, and keep probing voicego in the background.

**Licensing note.** voicego is AGPL-3.0-or-later (`refs/voicego/README.md`). gorca implements the client from the protocol specification and does not vendor or link voicego code.

### 8.3 Failure modes

| Failure | Detection | Behaviour |
|---|---|---|
| Server not reading (socket full) | write would block; SPSC full | Supersede queued commands with the latest `Replace`; `Degraded` after 50 ms; reconnect after 2 s |
| Server accepts but never starts | no `Started` within 200 ms of `Accepted` | count, health penalty, `Degraded`; TBD-V6 failover policy |
| Server crash | EOF | `Disconnected` and backoff (50 ms → 2 s) |
| Event flood (marks) | event channel full | coalesce `AudioProgress` and `Mark`; never drop `Ended` or `Stopped` |

Braille is unchanged: BrlAPI, same epoch, and its own ADR later.

PipeWire buffer tuning is **out of scope** for gorca. It belongs to the speech service.

## 9. Web document mirror and the SourceLayer (R6 extended)

### 9.1 Layering

```
           Reader (lock-free reads of DocSnapshot)
                         ▲
        ┌────────────────┴───────────────────┐
        │ Doc actor: document mirror          │  gorca-doc (pure data: flat text, keyed forest,
        │  apply(DocDelta) → publish snapshot │  indexes, diff, live-region classifier)
        └────────────────┬───────────────────┘
                 SourceLayer boundary (gorca-doc-source-api)
        ┌────────────────┼──────────────────────────────┐
   AtspiDocSource    CdpDocSource (TBD-S1)      WebExtDocSource (TBD-S2)
   (in gorca-        Chromium, DevTools          Firefox WebExtension +
   connector-atspi)  Accessibility domain        native messaging host
        └──────────── HybridDocSource (TBD-S4): composition of two of the above ────┘
```

The document mirror owns the model. A source only produces deltas and executes requests. Which source feeds which browser is decided by bake-off B-6, and web-a11y-engineer is researching feasibility in parallel. The AT-SPI source is mandatory and is always available as the fallback, because the user requires flawless Firefox out of the box (WEB §4.8).

### 9.2 Interface boundary

```rust
// crate gorca-doc-source-api — depends on gorca-model only. No tokio types in signatures
// except the channel types re-exported from gorca-rt.

pub trait DocSource: Send + 'static {
    fn kind(&self) -> SourceKind;                  // Atspi | Cdp | WebExt | Hybrid
    fn caps(&self) -> SourceCaps;
    /// Runs until detached or shutdown. Must honour deadlines on every DocRequest.
    fn attach(self: Box<Self>, ctx: DocSourceCtx) -> BoxFuture<'static, Result<(), SourceError>>;
}

pub struct DocSourceCtx {
    pub doc: DocId,
    pub sink: DeltaSink,                            // non-blocking push; coalesces per SrcNodeId
    pub requests: mpsc::Receiver<DocRequest>,       // bounded, every request has a deadline
    pub join: JoinPort,                             // identity reconciliation (9.3)
    pub shutdown: CancellationToken,
    pub clock: Clock,
}

pub struct SourceCaps {
    pub bulk_snapshot: bool,      // can deliver a whole document in one/few messages
    pub push_deltas: bool,        // pushes mutations itself (no re-query needed)
    pub settled_markers: bool,    // emits Settled after a mutation batch
    pub text_unit: TextUnit,      // Utf16Bom | Scalar | Utf8; mirror converts (TU-2)
    pub actions: ActionCaps,      // focus, click, scroll-into-view, set-caret, set-selection
    pub join_keys: JoinKeyCaps,   // which join keys it can provide (9.3)
}

pub enum DocDelta {
    Reset    { root: SrcNodeId, nav_id: u64 },                       // new document / navigation
    Upsert   (Vec<SrcNode>),                                         // role, states, name, attrs, ...
    Children { parent: SrcNodeId, children: SmallVec<[SrcNodeId; 8]> }, // ordered keyed child list
    Text     { node: SrcNodeId, edit: TextEdit },                    // Splice{range, text} | Replace(text)
    Removed  (Vec<SrcNodeId>),
    Settled  { batch: u64 },
    Hint     (DocHint),                                              // RouteChanged{url}, Scrolled, Visited(..)
}

pub enum DocRequest {
    Subtree  { root: SrcNodeId, max_nodes: u32, deadline: Instant, reply: Reply<Vec<DocDelta>> },
    Text     { node: SrcNodeId, range: Range<u32>, deadline: Instant, reply: Reply<String> },
    Action   { node: SrcNodeId, act: DocAction, deadline: Instant, reply: Reply<()> },
    Validate { nodes: SmallVec<[SrcNodeId; 4]>, deadline: Instant, reply: Reply<Vec<bool>> },
}
```

Source obligations (conformance-tested in `gorca-testkit`, like C-1…C-6):

- **SL-1** `sink.push` never blocks. Overflow marks the document dirty, and the doc actor runs a `Subtree` resync.
- **SL-2** Each request is answered or dropped by its deadline plus 5 ms.
- **SL-3** All source data is untrusted. Malformed messages are dropped and counted. A crash loop detaches the source (IR-7).
- **SL-4** `Children` deltas are complete ordered lists, so the mirror can do a keyed ordered-forest diff (WEB §4.4).
- **SL-5** A source never emits focus or caret. Those always come from the app's connector (AT-SPI), because they must be correct even when the source is detached.

### 9.3 Identity reconciliation requirements

The mirror has its own identity, `DocNodeId` (doc-local `u32`, never reused within a `nav_id`). Each source maps its native ids to `DocNodeId` through a bimap that the doc actor owns.

- **IR-1 One logical node, one `DocNodeId`.** When two sources describe the same node (hybrid), the mirror holds one entry. The fields carry provenance: which source last wrote them.
- **IR-2 Hot-path join.** Every AT-SPI focus, caret or selection event inside the document must resolve to a `DocNodeId` in O(1), with no IPC (p99 < 10 µs). If the active content source is not AT-SPI, the join from AT-SPI object reference to `SrcNodeId` must be built **during the document build or update**, never lazily on focus.
- **IR-3 Never speak the wrong node.** If the join misses, the Reader presents from the AT-SPI connector's snapshot (§5.2) and marks the document for resync of that subtree. The miss is counted (M-13).
- **IR-4 Deterministic keys first.** A join key is either exact, meaning the same id from both transports (for example a DOM or backend node id exposed in AT-SPI attributes), or heuristic (ancestor path, role, name, index in parent, bounds). A heuristic join must be confirmed by a `Validate` request or by a later event before it is used for actions. Heuristic joins are counted separately.
- **IR-5 Action routing.** An action goes to the source that declared it in `caps.actions`. Its effect is confirmed by the AT-SPI focus or caret event within the Interactive deadline. Otherwise it is retried once through AT-SPI.
- **IR-6 Units.** Each source declares `text_unit`. The mirror converts at apply time (TU-2).
- **IR-7 Degradation without losing place.** If a source detaches (extension removed, CDP pipe closed, crash loop), the document falls back to `AtspiDocSource`. The virtual caret is kept as `(DocNodeId, offset)` if the node joins, and otherwise as a text anchor: `(ordinal, 32-char context hash)` relocated by the NVDA-style bookmark rule.
- **IR-8 Trust and exposure.** CDP is fully privileged. It is reached only through `--remote-debugging-pipe` or an authenticated Unix socket, never through an open TCP port. Data from WebExtension content scripts can be influenced by page script and is treated as untrusted like any app data (SL-3).

**TBD hooks** (web-a11y-engineer research, then B-6):

- **TBD-S1 Chromium own-protocol source.** CDP `Accessibility` domain (full AX tree plus node update events) over a pipe. Who launches Chrome with the pipe, and does this survive normal user launches? What is the join key to the AT-SPI object path?
- **TBD-S2 Firefox own-protocol source.** WebExtension plus native-messaging host: message schema, batching and `Settled` markers, privileged pages that extensions cannot see, and the join key.
- **TBD-S3 Join keys per browser.** Does either browser expose an id in AT-SPI attributes or object paths that equals an id available to CDP or the WebExtension? If not, heuristic join cost and error rate.
- **TBD-S4 Hybrid split.** Which fields come from which source? For example: structure and text from the own protocol; focus, caret, actions and live attributes from AT-SPI.

### 9.4 Document mirror behaviour (resolves W-1…W-4 with WEB §4)

- **Build:** P2P, resolve the showing document (`STATE_SHOWING`; skip background tabs), warm Firefox cache domains once, then focus neighbourhood → viewport → breadth-first remainder. Requests are pipelined (depth 16 per app by default, bounded by the Prefetch class). Decoding and index building run on `cpu`. A snapshot is published after the focus neighbourhood is in (first-useful), and then every 50 ms or 2,000 nodes. While coverage is partial, navigation falls back to `Collection.GetMatches` (one round trip, about 0.3 ms per 156 nodes, WEB §1.6).
- **Updates:** dirty-subtree set with ancestor dominance (NVDA `invalidateSubtree`), 50–100 ms debounce for mutations, no debounce for focus and caret, subtree re-fetch and keyed child diff, and a single atomic snapshot publish per batch. If the document's event queue overflows, the whole document is marked dirty and rebuilt.
- **Indexes:** a document-order structure with O(log n) "next X after node" for each structural-navigation type. The data structure choice goes to math-researcher (WEB §4.9).
- **Live regions (W-3):** the `notification` signal maps directly to `Announcement`. Insertions are classified by cached `container-live`, `container-atomic`, `container-relevant` and `container-busy` attributes, and the announcement text comes from the mirror diff, not from the raw event. No app-specific code is needed; the policy and rate limits live in `gorca-doc`.
- **Placement (W-1):** browser quirks that are normalisation (offsets, text-leaf shape, caret overload, cache warm-up) live in the AT-SPI connector and `AtspiDocSource` (§5.6). Behavioural policy (auto focus-mode rules, Electron detection, site hacks) lives in `ext-firefox` and `ext-chromium`.

## 10. Extension model

| Tier | Runs as | Isolation | Can touch |
|---|---|---|---|
| Core policy | Rust in `gorca-core` | none | everything; must be connector- and app-neutral |
| Extension (WASM) | wasmtime component, one `Store` per (extension, app) | sandbox, fuel/epoch interruption, memory limit, trap → instance reset | only the WIT world below |
| Extension (native first-party), only if B-4 requires it | same Rust trait, linked in, run in the app actor | panic caught at the app-actor boundary; no hang protection | same API as WASM |

```wit
package gorca:ext@0.1.0;

interface types {
  type node-key = tuple<u64, u64>;
  record node { key: node-key, role: role, states: u64, name: option<string>, /* ... */ }
  enum budget { interactive, prefetch, background }
  variant query-error { timeout, gone, not-supported, denied }
}

interface host {
  use types.{node, node-key, budget, query-error};
  get-node: func(key: node-key) -> option<node>;                                  // mirror only, never blocks
  fetch: func(key: node-key, mask: u32, b: budget) -> result<node, query-error>;  // host-enforced deadline
  text: func(key: node-key, start: u32, end: u32, b: budget) -> result<string, query-error>;
  speak: func(segments: list<segment>, prio: prio);                              // tagged with current epoch
  set-setting: func(k: string, v: string);                                        // own namespace only
  log: func(level: u8, msg: string);
}

interface hooks {
  use types.{node};
  on-event: func(ev: event) -> event-verdict;                     // suppress / rewrite / request re-evaluation
  present: func(n: node, ctx: present-ctx) -> option<list<segment>>;
  nav-units: func() -> list<nav-unit>;
  nav-match: func(unit: u32, n: node) -> bool;
  command: func(id: u32) -> command-result;
}

world extension { import host; export hooks; }
```

The manifest declares a matcher (`app_name`, `toolkit`, `connector`, `document-role` globs), hooks, keymap and capabilities (`speak`, `fetch:prefetch`, `fetch:background`, `settings`). No raw IPC, no file system beyond a settings KV, no network. Limits: 2 ms CPU for `on-event` and `present`, 20 ms for `command`, 32 MiB per instance; three resets in 60 s disable the extension for that app, and the user is told once. Order when several match: app-specific, then toolkit-specific, then connector-specific; `present` takes the first `Some`, and `on-event` verdicts compose left to right.

Additions in this revision:

- `get-node` reads the mirror lock-free (§5.2). Hook calls run on `workers`, in the app actor's context, never on the Reader lane.
- Extension stores for different apps run in parallel.
- Planned first-party extensions: `ext-gnome-shell`, `ext-vte` (T-2 policy), `ext-firefox`, `ext-chromium`. If two extensions need the same workaround, it moves into a connector or the core.

## 11. Workspace layout (final, for scaffolding)

```
Cargo.toml                    # [workspace], resolver = "3", edition 2024, shared [workspace.lints]
rust-toolchain.toml           # pinned to the toolchain in .toolchain/
clippy.toml                   # disallowed-methods: zbus Proxy::call*, std::thread::sleep in async
crates/
  gorca-model/                # NodeKey, AppId, Role, StateSet, NodeSnapshot, NodeEvent, TextDelta,
                              # Utterance, Segment, Epoch, TextUnit. no tokio, no I/O.
  gorca-rt/                   # lanes (thread spawn, naming, rtkit priority), EpochCell, Deadline,
                              # BudgetClass, virtual Clock, bounded-channel re-exports, supervisor
                              # primitives, watchdog heartbeat
  gorca-ingress/              # coalescing ingress: CoalesceKey, ring + fold, SlotBudget (ADR-0004)
  gorca-mirror/               # MirrorRead / MirrorWriter, per-app lock-free snapshot store (MB-1)
  gorca-connector-api/        # Connector, ConnectorCtx, AppPort, BackendRequest/Op/Reply/Error
  gorca-doc-source-api/       # DocSource, DocSourceCtx, SourceCaps, DocDelta, DocRequest, JoinPort, DocNodeId
  gorca-doc/                  # document mirror: keyed forest, flat text, indexes, diff, live-region
                              # policy, bookmarks. pure data + rayon; no tokio, no I/O
  gorca-output-api/           # SpeechClient, SpeechCmd, SpeechEvent, SpeechCaps, Verbalized, BrailleBackend
  gorca-speech/               # SpeechQueue SM, speech lane, verbalizer (punctuation, symbols, dicts)
  gorca-speech-ssip/          # SSIP SpeechClient (speech-dispatcher; voicego text mode)
  gorca-speech-voicego/       # voicego native SpeechClient — skeleton with TBD-V1..V6 markers
  gorca-input-api/            # KeyEvent, KeySource, KeyInjector, GrabSet, KeyClass
  gorca-input-mutter/         # org.freedesktop.a11y.KeyboardMonitor client, name ownership (ID-1), MonitorState SM
  gorca-input-inject/         # KeyInjector over RemoteDesktop/EIS (libei); expectation table
  gorca-dbus/                 # deadline call helper, event-connection raw drain (ADR-0004), P2P pool,
                              # match-rule/RegisterEvent management, SIOCINQ sampling
  gorca-connector-atspi/      # AT-SPI connector, toolkit normalisation tables, AtspiDocSource,
                              # org.a11y.Status enablement (ID-5)
  gorca-core/                 # Reader, app actors, doc actors, Focus/Mode SMs, presenter, PresentJob
  gorca-braille-brlapi/       # stub until the braille ADR
  gorca-ext-api/              # WIT files + Rust trait mirror + bindings
  gorca-ext-host/             # wasmtime embedding, limits, manifests
  gorca-daemon/               # binary `gorca`: config, wiring, comm=orca (ID-2), signals (ID-3),
                              # systemd user unit
  gorca-testkit/              # fake connector, fake DocSource, scripted apps, virtual clock,
                              # conformance suites (C-*, SL-*), storm generator, private-bus spawner
  gorca-bench/                # criterion microbenches (MB-*) + M-1/M-3/M-11 harness entry points
xtask/                        # `cargo xtask deps` (dependency rules below), CI helpers
extensions/                   # WASM extensions; scaffold the directory and ext-vte skeleton only
  ext-vte/
tools/                        # Go/Python tooling (existing)
```

**Not scaffolded now:** `gorca-connector-newton` (R5); `gorca-doc-source-cdp`, `gorca-doc-source-webext` and `browser/firefox-webext/` (created by bake-off B-6 arms); `ext-gnome-shell`, `ext-firefox`, `ext-chromium` (created when their features start).

**Dependency rules** (enforced by `cargo xtask deps` in CI over `cargo metadata`):

1. `gorca-model`, `gorca-ingress`, `gorca-mirror`, `gorca-connector-api`, `gorca-doc-source-api`, `gorca-doc`, `gorca-output-api`, `gorca-input-api`, `gorca-core`, `gorca-speech` must not depend, even transitively, on `atspi*`, `zbus*`, `accesskit*`, `wayland-*`, `wasmtime*` or `reis`/libei bindings.
2. Only `gorca-dbus`, `gorca-connector-atspi`, `gorca-input-mutter`, `gorca-input-inject` and `gorca-daemon` may depend on `zbus`. Only `gorca-connector-atspi` may depend on `atspi*`.
3. `gorca-core` must not depend on any `gorca-connector-*`, `gorca-speech-*`, `gorca-input-*` (except `-api`) or `gorca-ext-host`. Only `gorca-daemon` wires concrete implementations.
4. `gorca-model` and `gorca-doc` must not depend on `tokio`.
5. Extensions depend only on `gorca-ext-api`.
6. Workspace lints: `clippy::unwrap_used = "deny"`, `clippy::expect_used = "deny"` and `clippy::panic = "deny"` outside `#[cfg(test)]`; `clippy::wildcard_enum_match_arm = "deny"` in crates holding state machines (`gorca-core`, `gorca-speech`, `gorca-input-mutter`, `gorca-rt`); `unsafe_code = "deny"` except in `gorca-dbus` (SIOCINQ) and `gorca-rt` (prctl, sched), where it is allowed per module with a `// SAFETY:` comment.
7. Release profile: `panic = "unwind"` (supervised restarts), `lto = "thin"`, `codegen-units = 1`.

## 12. Alternatives considered

| Alternative | Why not |
|---|---|
| Orca-style single loop with synchronous IPC | Root cause of Orca's freezes (`refs/at-spi2-core/dbind/dbind.c:66-130`). |
| Single global actor with async handlers (Odilia) | Serialises all apps behind the slowest. |
| Live proxies as the object model | Every property read becomes IPC; latency cannot be bounded. |
| Callback-trait connector | Couples core latency to connector internals; cannot move out of process. |
| **Shared-lock mirror (`RwLock<HashMap>`) read by the Reader** | Writer bursts during tree builds would stall the Reader. PC-1 snapshots give wait-free reads at a memory cost we accept. |
| **Parallel/sharded Reader** (for example one Reader per app) | Focus and speech are global per user. Sharding would need cross-shard ordering for focus moves between apps, which recreates Odilia's lost-ordering bugs. The audit (§3.3) shows the serial part can stay small. |
| **One zbus connection for everything (Odilia, atspi-rs)** | Method replies queue behind the event flood on the same socket (F-3), and zbus's 64-message queue backpressures into the broker, which then disconnects us. |
| **Speech in-process (espeak-ng → PipeWire), former B-3 arm b** | Rejected by the owner (R4): a separate service gives crash isolation and lets voicego own audio. The cost is accepted: one extra IPC hop (~0.1 ms) and server-side cancel latency we do not control (F-5). |
| **Speech queue kept in the server (SSIP priorities only)** | Remote, coarse cancellation; `important` cannot be cancelled by `message` (`refs/speech-dispatcher/doc/ssip.texi:571-593`). |
| **`GrabKeyboard` and re-inject everything not a command (NVDA-like full interception)** | Every typed key would take an extra injection hop, and IME, repeat (dropped by Mutter for virtual devices) and modifier state would become fragile. Minimal grab sets (K-1) avoid this. |
| **AT-SPI-only web mirror, no SourceLayer** | Possibly too slow for 10k+ node pages (F-7 naive 12–16 s). It stays as the mandatory fallback, and B-6 measures it. |
| Lua/Python extensions; out-of-process extensions | Scripting has no hard CPU or memory limits and GC pauses; out-of-process hooks cost a context switch each, too slow for `present`. Out-of-process stays an option for untrusted third-party extensions later. |

## 13. Consequences

**Positive**

- A hung or flooding app affects only its own actor. The Reader stays responsive by construction: no awaits, lock-free reads, biased select, dedicated thread.
- The socket drain design makes broker disconnection, which would be fatal, structurally avoidable and measurable (M-11).
- Cancellation is one atomic store away from the key press (K-3, E-2).
- Bulk work scales with cores: documents, apps, decoding and indexing all run in parallel.
- The speech server is replaceable and can crash without taking gorca down.
- The web source is replaceable per browser without touching the mirror, indexes or Reader.

**Negative / costs**

- gorca cannot run next to Orca, and it changes the system's view of "orca" (ID-3): `pgrep orca`, `orca --replace` and Chromium detection all see gorca. Users switching between them need a clear story.
- RCU snapshots cost memory and allocation churn on large documents. This is accepted (PC-6) and bounded by M-8.
- More threads mean scheduling noise on small machines. B-5 measures it.
- Hot-lane SCHED_FIFO needs rtkit. Without it, the hot lanes run at normal priority and B-5's numbers must still hold.
- Speech latency now depends on an external server. gorca can guarantee only its own part (M-1); M-12 tracks the end-to-end number.
- The hybrid web source adds identity reconciliation, a new class of bug (wrong-node). IR-3 and M-13 guard it.
- Browse-mode grab sets have an ambiguity window of a few ms during mode switches (§7.3). Ambiguous keys are counted, not guessed.

## 14. Metrics to validate

| ID | Metric | Target | How measured |
|---|---|---|---|
| M-1 | key press (input lane receipt) → first byte of `Speak` written to the speech socket, cache hit | p99 < 15 ms, p50 < 3 ms, with all cores busy with a document build | perf harness, fake connector, tracing spans |
| M-2 | same, cache miss, app replying in 5 ms | p99 < 15 ms gorca-side (IPC excluded by span accounting) | fake AT-SPI app on a private bus |
| M-3 | sustained ingest | 10k ev/s for 60 s over 5 apps: no unbounded growth, Reader lag < 20 ms. 100k ev/s burst on 1 app: other apps' M-1 Δp99 < 1 ms | storm generator |
| M-4 | hung-app isolation | focused app stops replying: "not responding" < 500 ms after the first key press; M-1 elsewhere unaffected | fake hung app |
| M-5 | cancellation | key press or new focus → `Stop`/`Replace` written < 1 ms gorca-side; 0 stale utterances audible in 10k randomized trials | trace assertions |
| M-6 | refine/debounce quality | `may_refine` corrections < 1 % of focus presentations; no double speech | recorded session replays |
| M-7 | extension isolation | trap or infinite loop in `present`: recovery < 50 ms; hook overhead p99 < 100 µs | ext-host bench |
| M-8 | memory guard rail | no unbounded growth over 8 h soak; RSS < 600 MB with Firefox (10 tabs, one 100k-node page) + Terminal + Shell | smem in a scripted session |
| M-9 | browse-mode build | 10k-node page: first line speakable < 150 ms after load-complete; full mirror < 2 s in the background | web harness |
| M-10 | Newton parity | deferred with R5 | — |
| **M-11** | drain health | broker never disconnects gorca during 10 min of `yes \| head -c 1G`-class terminal output plus a 100k ev/s synthetic storm; socket backlog (`SIOCINQ`) p99 < 1 MiB; drain thread CPU < 60 % of one core | storm generator + real VTE on a private session |
| **M-12** | end-to-end speech latency (diagnostic, not a gorca gate) | key press → audio at sink, reported per backend (voicego native, voicego SSIP, speech-dispatcher); cancel → silence | loopback capture |
| **M-13** | identity reconciliation | focus/caret join hit rate ≥ 99.9 % on the `tests/web/` corpus; 0 wrong-node presentations; heuristic-join share reported | B-6 harness |
| **M-14** | Reader cost | per-message p99 < 100 µs; occupancy < 10 % of one core during M-3 | tracing + `perf sched` |
| **M-15** | parallel build scaling | parse + index of a 100k-node bulk snapshot < 300 ms on 8 cores; speedup ≥ 4× from 1 to 8 cores | `gorca-bench` |
| **M-16** | key-grab correctness | 0 lost or doubled keys in 10k randomized mode switches during typing; ambiguous-key count reported | input harness with a private Mutter (debug env var) |

Microbenchmarks (perf-engineer, no bake-off): **MB-1** mirror store read and write under contention (§5.2); **MB-2** `EpochCell` to speech-lane wakeup latency; **MB-3** drain header-peek cost per message.

## 15. Bake-offs (final list, priority order)

| Prio | ID | Decision | Arms | Judge on |
|---|---|---|---|---|
| 1 | ~~**B-2**~~ **Resolved → ADR-0004** (A wins: 80 vs C 77, B 61) | Event drain + ingress coalescer (extended) | (a) zbus `MessageStream` on a drain thread + `Mutex<IndexMap>` coalescer; (b) own raw D-Bus framing reader on a drain thread + lock-free MPSC + generation-tagged lazy dedupe in the app actor; (c) (b) plus sharded event connections with `sender=` match rules for heavy apps | M-11 (no disconnect, backlog), M-3, p99 push cost, drain CPU, M-1 under storm |
| 2 | **B-5** | Runtime topology (upgraded from "measure only" to a measured A/B; same code, config switch, no separate implementations) | (a) everything on one tokio multi-thread runtime; (b) hot lanes on dedicated threads + workers + rayon (§3.2); (c) (b) plus rtkit `SCHED_FIFO` and core pinning for hot lanes | M-1 and M-5 while M-3 runs **and** all cores are saturated by M-15 builds; M-14 |
| 3 | **B-1** | Native-app mirror fill strategy | (a) lazy on demand + invalidation; (b) eager `GetItems` per app; (c) hybrid: eager focus-window ancestry and siblings + lazy elsewhere. All arms use P2P and the B-2 winner | M-1/M-2 hit rate, D-Bus msgs/s, M-8, first-focus latency in Shell and Terminal |
| 4 | **B-6** | Web SourceLayer (new; starts after web-a11y-engineer's feasibility report fills TBD-S1…S4) | (a) AT-SPI only, pipelined P2P; (b) own protocol only where available (CDP for Chromium, WebExtension for Firefox) with AT-SPI for focus and caret; (c) hybrid per TBD-S4 | M-9, M-13, M-15, robustness (source detach, IR-7), setup friction (flags, extension install), security exposure (IR-8) |
| 5 | **B-4** | First-party extension execution | (a) WASM only; (b) dual native + WASM | M-7, `ext-vte` under T-1 storm, browse build time |
| — | ~~B-3~~ | Speech backend | **Retired** by R4 (separate service, voicego primary). Replaced by diagnostic M-12. | — |

B-2 (now resolved by ADR-0004) came first because a broker disconnect is the one failure that loses everything. It also feeds B-5's load generator. B-5 is cheap, a configuration switch over the same actors, and fixes the threading layout before others tune against it. B-1 serves the first targets in the polish order (Shell, then Terminal). B-6 waits for research.

## 16. Open questions

### 16.1 Resolved by the platform report

| ID | Resolution |
|---|---|
| P-1 | Contract known (`docs/platform/org.freedesktop.a11y.KeyboardMonitor.md`). Keys can be consumed only through the grab set or `GrabKeyboard`; the event is unicast with no ack and no pass-through deadline; Mutter never waits. Authorisation is by owning `org.gnome.Orca.KeyboardMonitor` → ID-1, §7.3. True end-to-end latency is unmeasured (P-10). |
| P-2 | Broker-side filtering by match rule (interface, member, sender) works; apps filter by `Registry.RegisterEvent`, except always-on cache-maintenance events. There is no broker-side throttling → D-5. |
| P-3 | Unlimited per-uid quota; slow receivers are buffered and then **disconnected**, never dropped → D-1…D-3, M-11. |
| P-4 | Partly. `GetItems` works for native apps (4–5 ms) and returns no web document nodes → §5.4, §9. GTK4 `AddAccessible`/`RemoveAccessible` consistency is still open → P-9. |
| P-5 | SSIP: cancel ack 0.09 ms; cancel + new → BEGIN 13.1 ms p50. Now relevant only to the SSIP compatibility backend and M-12. |
| P-6 | GNOME 48 keyboard path is access-controlled by name ownership. Newton's `KeyEvent` has the same signature, is broadcast, and has no access control → stays deferred (R5, N-1). |

### 16.2 Resolved by the web report

| ID | Resolution |
|---|---|
| W-1 | AT-SPI can build a mirror, but naive walking costs 1.25–1.6 ms per node. Pipelined P2P with a 0.3 ms/node target is unverified. A fast path is possible through SourceLayer → B-6. Placement: normalisation goes in the connector and `AtspiDocSource`, policy in extensions (§9.4). |
| W-2 | Safe keys: `children-changed` → dirty subtree with ancestor dominance; Firefox text events applied locally and verified lazily; Chromium static text re-fetched on `children-changed`; focus and caret never debounced; Chromium caret(0) on a document is scroll-to-anchor (§5.6, §9.4). |
| W-3 | Yes, without app-specific code. `notification` → `Announcement`; other events are classified by cached `container-*` attributes and the text comes from the mirror diff (§9.4). |
| W-4 | AT-SPI references are stable per Accessible (Firefox) and per AX node (Chromium). Chromium may rebuild the tree on navigation (→ `DocDelta::Reset`). Firefox may drop hide events (→ I-1 via defunct replies). |

### 16.3 Still open

| ID | Owner | Question |
|---|---|---|
| P-7 | linux-platform-engineer | Ordering between the `SetKeyGrabs` reply and the moment Mutter's input thread applies the grabs. Is the 5 ms settle in §7.3 enough? Needs measurement with the debug env var. |
| P-8 | linux-platform-engineer | Do keys injected through RemoteDesktop/EIS come back as `KeyEvent`, and with what latency? Does Mutter 48.7 check more than session ownership for non-portal RemoteDesktop (`CreateSession` succeeded unprivileged)? |
| P-9 | linux-platform-engineer | GTK4 consistency of `AddAccessible`/`RemoveAccessible` (feeds B-1). |
| P-10 | linux-platform-engineer | End-to-end key → `KeyEvent` latency with gorca owning the name. |
| P-11 | linux-platform-engineer | Does atk-bridge (GTK3/VTE) honour the per-app `app` argument of `RegisterEvent`, so that D-5 deregistration works per app? |
| P-12 | linux-platform-engineer | rtkit limits (`RTTimeUSecMax`, max priority) on Debian 13 for a user service; whether the RT kernel needs extra configuration for SCHED_FIFO hot lanes. |
| W-5 | web-a11y-engineer | TBD-S1…S4 (§9.3). |
| W-6 | web-a11y-engineer | Does `comm=orca` (ID-2) put Chromium into `kAXModeComplete | kScreenReader` as expected? Compare tree and events with `--force-renderer-accessibility` alone. |
| W-7 | math-researcher | Document-order index, keyed forest diff break-even, flat-text offset map (WEB §4.9 items 1, 2 and 4). |
| V-1 | go-engineer | TBD-V1…V5 (§8.2). |
| N-1 | none (deferred, R5) | Newton security: Manager has no sender checks, `KeyEvent` is broadcast. No dependency until an access-controlled channel exists. |
| N-2 | none (deferred) | Wire format pinned to AccessKit 0.16 JSON; current AccessKit 0.25 uses `deny_unknown_fields`. |
| N-3 | none (deferred) | `LocalId` scheme for `(tree_id, node_id)`; GTK text-run ids are unstable, so cursors anchor to container + offset. |
| N-4 | none (deferred) | No app identity (surfaces only), so dual exposure cannot be deduplicated and extensions cannot match by app name. |

## 17. Sign-off

### 17.1 Accepted (2026-10-12, recorded in `docs/decisions.md`)

- **R1** Orca impersonation: bus name and process name; Chrome `--force-renderer-accessibility`; upstream fixes later (§5.5).
- **R2** Message-based connector boundary with a mandatory per-request deadline (§5).
- **R3** Non-awaiting Reader with epoch-based cancellation (§3.3, §3.4, §7).
- **R4** Speech is a separate service; `SpeechClient` with voicego-native (primary) and SSIP (compatibility) backends; gorca owns the queue and cancellation; no in-process synthesis; no PipeWire tuning (§7.4, §8).
- **R5** No Newton runtime dependency; connector API ready; connector deferred (§5.7).
- **R6** Per-document web mirror, viewport-first, with a SourceLayer decided by bake-off B-6 (§9).
- **Performance philosophy:** speed over frugality, fully multi-threaded (§3).

### 17.2 New decisions requiring sign-off

1. **ID-3 handling:** `RestartPreventExitStatus=SIGKILL` (gorca does not fight `orca --replace`) and a SIGUSR1 handler (§5.5).
2. **ID-1 startup with Orca running:** queue for the name, announce once, run without key commands until acquired (§5.5).
3. **TBD-V6 speech failover:** automatic fallback from voicego to speech-dispatcher (SSIP), announced once (§8.2).
4. **K-1/K-2 key capture:** minimal grab sets, with re-injection only for swallowed keys meant for the app, instead of `GrabKeyboard` plus re-injecting everything (§7.3).
5. **K-3 interrupt-on-keypress on the input lane:** any non-modifier key press silences speech before the Reader runs (configurable) (§7.3).
6. **D-5 per-app event deregistration** for flooding background apps, if P-11 confirms that it works (§5.3).
7. **M-8 relaxed** from 150 MB to a 600 MB guard rail plus "no unbounded growth" (§14).
8. **B-5 upgraded** to a measured A/B, and **B-3 retired** (§15).
