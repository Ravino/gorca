# ADR-0001: Core architecture — connector-neutral engine, per-app actors, cancellable output

- **Status:** Proposed
- **Date:** 2026-10-12
- **Deciders:** project owner (sign-off required on the items in "Decisions requiring sign-off")
- **Supersedes:** none
- **Related:** `docs/reports/2026-10-12-architect-prior-art.md` (failure catalog for Orca, Odilia lessons, Newton analysis)

## 1. Context

gorca replaces Orca. The goals in `CLAUDE.md` are hard constraints:

| Goal | Constraint it puts on the architecture |
|---|---|
| p99 < 15 ms gorca-side keypress → speech start | No synchronous IPC on the keypress path when a cached answer exists; the presentation step must work from a local snapshot. |
| Never block | Every IPC call carries a deadline. No component may wait on another component without a deadline. |
| 10k+ events/s | Events are coalesced before any expensive work. Ingest is O(1) per event and allocation-light. |
| Crash isolation | Per-app handling and extensions run behind an isolation boundary that a panic, trap, hang or flood cannot cross. |
| Backend-agnostic core | The core never names AT-SPI, D-Bus, zbus, AccessKit or Wayland types. |
| GNOME-only, not GNOME-coded | GNOME specifics live in connectors and extensions. |

The prior-art report shows that Orca's instability comes from structure rather than from individual bugs:

- **One thread and synchronous IPC.** Orca runs on one thread and makes synchronous AT-SPI calls inside event handlers. libatspi re-enters the main loop while it waits for a reply (`refs/at-spi2-core/dbind/dbind.c:66-130`). Its default timeout is 800 ms per call (`refs/at-spi2-core/atspi/atspi-misc.c:191`), so a single hung app stalls the whole reader for multiples of 800 ms.
- **Ad-hoc event pruning.** The event queue has no per-app isolation.
- **Implicit app logic.** Toolkit and app special cases are scattered through generic code.

Odilia showed that Rust, zbus and the `atspi` crate are a workable base. Its problems:

- No D-Bus call has a deadline.
- One serial cache actor serves every app.
- There is no speech cancellation.
- Modes and navigation are placeholders.

It is still "Beta, not production ready" (report §2). zbus offers only a connection-wide `method_timeout`, and `Proxy::call_with_flags` bypasses even that (`refs/zbus/zbus/src/connection/builder.rs:465-474`, `refs/zbus/zbus/src/proxy/mod.rs:871-899`). atspi-rs's `ObjectRef` decoding `assert!`s on malformed input (`refs/atspi-rs/atspi-common/src/object_ref.rs:442`). Deadlines and defensive decoding are therefore gorca's job.

Newton (Matt Campbell, last activity 2024-06) is a Wayland-native path for AccessKit trees:

- **Apps** push serde-JSON `TreeUpdate`s per `wl_surface` over a staging Wayland protocol.
- **Mutter** forwards them as pipe fds to the AT over the session-bus interface `org.freedesktop.a11y.Manager` / `UpdateSink` (`refs/newton/mutter/data/dbus-interfaces/org.freedesktop.a11y.xml`).
- **The AT** derives events itself by diffing trees.

It has no app identity (surfaces only), no security model, and a wire format pinned to AccessKit 0.16, whereas current AccessKit is 0.25 (report §3). gorca must be able to adopt Newton without changing the core, and should expect its protocol to change.

NVDA supplies two patterns we adopt:

- **Validity-checked cancellable speech.** `FocusLossCancellableSpeechCommand`, `refs/nvda/source/eventHandler.py:186-218`, re-checked at push time in `refs/nvda/source/speech/manager.py:483-500`.
- **A watchdog that never lets one app freeze the core.** See `refs/nvda/source/watchdog.py:47-56` and `cancellableExecute` at `:406`.

NVDA's in-process virtual buffer (`refs/nvda/nvdaHelper/vbufBackends/gecko_ia2`) is the bar for Firefox. We cannot inject into apps on Linux, so our document model must be built from out-of-process queries. That is the main performance risk.

## 2. Decision (summary)

1. **Engine plus connectors.** gorca is a library engine, `gorca-core`, plus a thin daemon. Accessibility sources are **connectors** that implement a message-based boundary (§4). AT-SPI2 and Newton connectors can run at the same time. Each *application* is owned by exactly one connector.
2. **Connector-neutral object model.** Identity is a `NodeKey` scoped by connector, app and app incarnation (§3). Nodes are snapshots, not live proxies. A stale key resolves to `Gone`; it never resolves to the wrong object.
3. **One actor per application.** Each actor owns its mirror cache, its coalescer, its health state machine and the extension instances bound to that app (§5). The single **Reader** actor owns all user-visible state machines: focus, mode, navigation and the speech intent. It never awaits IPC. It sends queries to app actors and handles the replies as messages.
4. **Bounded, coalescing ingress with explicit overload modes.** The connector ingress never blocks. Overflow converts events into "dirty" marks that are resynced later (§5.3).
5. **Deadlines everywhere** (§5.4), by budget class. An app health state machine acts as a circuit breaker.
6. **Speech queue state machine with epoch-based cancellation.** Gorca keeps at most one utterance in flight to the synthesizer, so cancellation is local and immediate (§7).
7. **Extensions are WASM components** behind one WIT world, with capabilities granted per extension. First-party extensions may also be compiled natively against the same Rust trait. Whether to do that is decided by a bake-off (§8).
8. **Workspace layout with dependency rules enforced in CI** (§9).

## 3. Core object model

### 3.1 Identity

```rust
/// Registered connector instance (atspi, newton, fake). Assigned by core at startup.
pub struct ConnectorId(u16);

/// An "app scope" incarnation: the unit of isolation. AT-SPI: one bus unique name.
/// Newton: one surface id (Newton has no app identity; report §3.5).
/// A restarted app / recreated surface gets a new AppId. Never reused within a gorca process
/// lifetime (monotonic u32 + connector).
pub struct AppId { connector: ConnectorId, serial: u32 }

/// Connector-local node handle. Opaque to core. AT-SPI: index into the connector's interner of
/// (bus_name, object_path). Newton: AccessKit NodeId (u64) — see open question N-3.
pub struct LocalId(u64);

/// Stable, copyable, hashable key. 16 bytes. The only way the core refers to a node.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct NodeKey { app: AppId, local: LocalId }
```

**Invariants**

- I-1: A `NodeKey` never denotes two different objects. AT-SPI may reuse object paths, and Newton may reuse NodeIds after removal. If the connector cannot guarantee freshness, it must allocate a new `LocalId` on reuse: it interns `(path, generation)`.
- I-2: The core holds no connector type. `LocalId` is opaque, and only the owning connector interprets it.
- I-3: Resolving a key whose app is `Gone`, or whose node was removed, yields `Lookup::Gone`. It never panics and never blocks.

### 3.2 Node snapshot

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
    pub attrs: AttrMap,             // small, interned keys (xml-roles, level, tag, ...)
    pub relations: Relations,       // labelled-by, controls, flows-to, ... (keys)
    pub bounds: Option<Rect>,
    pub fresh: Freshness,           // see 3.3
}
```

Text is **not** stored in the snapshot. Text goes through a `TextQuery`, such as `GetText(range)`, `CaretOffset` or `Attributes(range)`, because text is large and changes often. The Newton connector answers text queries from its local tree, the AT-SPI connector over IPC. This asymmetry is deliberate: the core does not care where an answer comes from.

### 3.3 Lifetime and freshness

```rust
pub enum Freshness {
    Fresh { at: Instant },           // confirmed by connector push or reply
    Stale { since: Instant },        // invalidated by an event; still usable for presentation with care
    Fetching { req: ReqId, deadline: Instant },
    Defunct,                         // connector reported removal / app gone; tombstone kept briefly
}
```

| From \ Event | invalidate | fetch issued | reply ok | reply timeout/err | removed |
|---|---|---|---|---|---|
| Fresh | Stale | Fetching | n/a | n/a | Defunct |
| Stale | Stale | Fetching | n/a | n/a | Defunct |
| Fetching | Fetching (mark re-dirty) | no-op (dedupe) | Fresh, or Stale if re-dirtied | Stale (+ health penalty) | Defunct |
| Defunct | Defunct | refused | dropped | dropped | Defunct |

Tombstones are evicted after N seconds, or when the app becomes `Gone`. The core may *present* a `Stale` node. If it does, it tags the utterance `may_refine`, and a later `Fresh` reply can replace the utterance if it is still current (§7).

### 3.4 Events (core vocabulary)

The connectors normalize raw events into a small closed set. Coalescing works on this set.

```rust
pub enum NodeEvent {
    FocusChanged { node: NodeKey },                       // focused state gained (connector derives for Newton)
    ActiveDescendantChanged { container: NodeKey, child: Option<NodeKey> },
    PropertiesChanged { node: NodeKey, mask: PropMask },  // name|description|value|role|states|bounds|attrs
    ChildrenChanged { node: NodeKey },                    // coarse: subtree structure dirty
    TextChanged { node: NodeKey, delta: TextDelta },      // Insert{offset,len,text?} | Delete{offset,len,text?} | Replaced
    CaretMoved { node: NodeKey, offset: u32 },
    SelectionChanged { node: NodeKey },
    Announcement { node: Option<NodeKey>, text: Arc<str>, politeness: Politeness }, // live regions, notifications
    WindowEvent { node: NodeKey, kind: WindowKind },      // activate/deactivate/create/destroy
    DocumentEvent { node: NodeKey, kind: DocKind },       // load-start/complete/reload
    NodeRemoved { node: NodeKey },
}
pub enum AppLifecycle { Appeared(AppInfo), Ready, Gone }
```

`AppInfo` carries `toolkit`, `app_name`, `pid` (if known) and `connector_kind`. Extensions match on these. The core itself never branches on toolkit or app name.

## 4. Connector boundary

The boundary is **message-based**, not a trait of callbacks, for three reasons. Connectors keep their own concurrency, the core cannot be re-entered, and a future out-of-process connector (for example Newton running in a sandboxed helper) keeps the same contract.

```rust
// crate: gorca-connector-api (depends only on gorca-model)

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

/// Given to the connector when it announces an app. Connector -> core direction.
pub struct AppPort {
    pub app: AppId,
    ingress: IngressSender,              // non-blocking, coalescing (see 5.3); try_push never awaits
    pub requests: mpsc::Receiver<BackendRequest>,  // core -> connector direction, bounded
}

impl AppPort {
    pub fn push(&self, ev: NodeEvent);                    // O(1), never blocks, may coalesce or mark-dirty
    pub fn push_snapshot(&self, nodes: SnapshotBatch);    // bulk cache fill (AT-SPI GetItems / Newton full tree)
    pub fn lifecycle(&self, ev: AppLifecycle);
}

pub struct BackendRequest {
    pub id: ReqId,
    pub deadline: Instant,               // mandatory; connector must reply or drop by then
    pub class: BudgetClass,              // Interactive | Prefetch | Background
    pub op: BackendOp,
    pub reply: oneshot::Sender<Result<BackendReply, BackendError>>,
}

pub enum BackendOp {
    Fetch { node: NodeKey, want: PropMask },
    Children { node: NodeKey, range: Range<u32> },
    Subtree { root: NodeKey, max_nodes: u32 },           // bulk; connector chooses strategy
    Text(TextQuery),
    Action(ActionReq),                                   // DoDefault, Focus, SetCaret, Select, ScrollTo, SetValue
    HitTest { point: Point },
    Collection(MatchRule),                               // structural nav server-side search where supported
}

pub enum BackendError { Timeout, Gone, NotSupported, Busy, Protocol(String) }
```

**Connector obligations** (checked by a conformance test suite in `gorca-testkit`):

- C-1: `push` is called from connector tasks only and never blocks. Per-call cost: < 1 µs p99.
- C-2: Every `BackendRequest` gets exactly one reply, or the connector drops the oneshot, by `deadline + 5 ms`. The core treats a dropped sender as `Timeout`.
- C-3: The connector runs any `Busy` throttling itself, for example a per-app in-flight limit on D-Bus.
- C-4: The connector maps raw roles and states to core `Role` and `StateSet` with a table, never with ad-hoc code. Unknown values map to `Role::Unknown` plus a raw attribute.
- C-5: Each app scope is owned by exactly one connector. **Dual exposure:** an app may be visible through both connectors. Newton today exposes no pid or app-id, so the registrar cannot correlate the two views automatically. Policy until Newton gains app identity:
  - The Newton connector is opt-in.
  - When it is enabled, the registrar applies a configured rule (for example "suppress AT-SPI apps whose toolkit is GTK ≥ X").
  - The registrar keys apps by `(connector, scope)` and never merges views.
  - See open question N-4.
- C-6: All app-supplied data is untrusted. Decoding errors or panics are contained in the connector. One malformed message drops that message, or that app scope, and is counted. It never takes down the connector task.

**AT-SPI connector specifics.**

- **Deadline helper.** Every call goes through one helper, `call(deadline, ...)`, which wraps `tokio::time::timeout`. Direct proxy calls are banned with `clippy::disallowed_methods`, because `call_with_flags` bypasses zbus's timeout. A connection-wide `method_timeout` of 2 s is a backstop only.
- **Own connection.** We build our own `zbus::Connection` with `max_queued` ≥ 4096 instead of using `atspi::AccessibilityConnection`. That type cannot be configured (`refs/atspi-rs/atspi-connection/src/lib.rs:103-122`), and its default `p2p` startup calls every app in sequence with no timeout (`p2p.rs:304-337`).
- **Demux task.** It must poll the `MessageStream` continuously (`refs/zbus/zbus/src/message_stream.rs:24`). It does no async work per event: it extracts the coalescing key from the borrowed message and routes by sender unique name to `AppPort`s.
- **Match rules.** We issue `AddMatch` and `RemoveMatch` ourselves, because atspi's `remove_match_rule` adds instead of removing (`atspi-connection/src/lib.rs:262`). Registration goes through `org.a11y.atspi.Registry` and the `Cache` interface (`GetItems` / `AddAccessible` / `RemoveAccessible`, `refs/at-spi2-core/xml/Cache.xml:16-98`). Open questions P-1 to P-4 are for the linux-platform-engineer.

**Newton connector specifics** (report §3.7):

- **Subscription.** A zbus proxy to `org.freedesktop.a11y.Manager`, plus one exported `UpdateSink` per subscribed surface. Each surface is an app scope.
- **Decoding.** Each update fd is read to EOF with a size cap and decoded by a **version-pinned** JSON decoder into an `accesskit_consumer::Tree` per surface.
- **Panic containment.** `Tree` panics on invalid updates (`refs/accesskit/accesskit_consumer/src/tree.rs:88-449`). Application of updates runs under `catch_unwind`, and on a panic the connector drops the tree and resubscribes to get a full update. If panics recur, the decoder and tree move to a helper process; the message boundary in this section already allows that.
- **Events.** Our own `TreeChangeHandler` emits `NodeEvent`s. Text diffs are computed **per text run**. AccessKit's atspi adapter diffs the whole document instead (`refs/accesskit/adapters/atspi-common/src/adapter.rs:121-182`), which is O(document) and unusable for VTE.
- **Queries.** `Fetch`, `Children`, `Subtree` and `Text` are answered locally as `Fresh`.
- **Actions.** `DoAction` is fire-and-forget, with no acknowledgement, so actions carry an "expect effect by deadline" timer.
- **Input.** Newton's `KeyEvent` / `SetKeyGrabs` can back an input `KeySource` (§9, `gorca-input-api`).

## 5. Actor topology

```
                ┌────────────────────────── gorca-daemon (supervisor) ──────────────────────────┐
 keyboard  ──►  │ Input actor ──(KeyIntent, prio)──►┐                                          │
 monitor        │                                   ▼                                           │
                │  ┌─────────────┐  AppEvent  ┌───────────┐  SpeechIntent ┌──────────────┐     │
 a11y bus ──►   │  │ AT-SPI conn │──►App actor│──────────► │  Reader    │ ─────────────► │ Speech actor │──► SSIP
 (zbus)         │  │ demux task  │  (1/app)   │◄──query────│  actor     │ ─BrailleIntent►│ Braille actor│──► BrlAPI
                │  └─────────────┘            └───────────┘ (focus/mode/│               └──────────────┘
 Newton   ──►   │  ┌─────────────┐            ┌───────────┐  nav SMs)   │
 socket         │  │ Newton conn │──►App actor│────────────►            │◄── ext replies ── Extension host(s)
                │  └─────────────┘            └───────────┘             │
                └──────────────────────────────────────────────────────────────────────────────┘
```

### 5.1 Actors and ownership

| Actor | Count | Owns | Never does |
|---|---|---|---|
| Supervisor | 1 | spawn/restart policy, health telemetry | any a11y logic |
| Input | 1 | key monitor connection, gorca keymap, modifier state | IPC to apps |
| Connector tasks | per connector | transport, interners, raw→core mapping | presentation |
| **App actor** | 1 per app | mirror cache (`NodeKey → NodeSnapshot`), coalescer, `AppHealth` SM, request multiplexer, bound extension instances | user-visible state |
| **Reader** | 1 | `FocusSM`, `ModeSM`, navigation cursors, presentation (generators), speech/braille *intents* | await anything; it is a pure message loop with a per-message CPU budget |
| Speech | 1 | `SpeechQueueSM`, SSIP connection SM | presentation |
| Braille | 1 | display regions, panning, BrlAPI connection | presentation |
| Extension host | 1 pool, instances per app | wasmtime engine, store per (extension, app) | direct IPC |

**Why the Reader does not await.** The Reader handles one message at a time and turns `(state, message)` into `(state', effects)`. The effects are sends: queries to app actors, intents to output. A query reply comes back as an ordinary message tagged with the `ReqId` and the focus epoch it was issued in. Replies from an old epoch are discarded. This is the structural fix for Orca's synchronous handlers. It also makes the Reader deterministic and replayable in tests.

**Runtime.** Tokio multi-thread runtime for I/O and app actors. The Reader and Speech actors run on a **dedicated current-thread runtime** on a dedicated OS thread with `SCHED_FIFO` (low priority) when permitted, so that app-actor CPU work can never delay keypress handling. This is bake-off candidate B-5, but low priority; it can be decided by measurement without a full bake-off.

### 5.2 Channels and backpressure

| Edge | Channel | Bound | When full |
|---|---|---|---|
| connector → app actor | **coalescing ingress** (5.3) | 4096 distinct keys | degrade to `Flooded` (mark-dirty, no queueing) |
| app actor → Reader | bounded mpsc, **focus-app lane + background lane** | 256 / 64 | background lane: drop oldest non-focus event and set `resync_needed`; focus lane: coalesce in app actor, never drop `FocusChanged` |
| Reader → app actor (queries) | bounded mpsc | 64 | `Busy` reply synthesized locally; Reader falls back to cached snapshot |
| app actor → connector (`BackendRequest`) | bounded mpsc | 32 | coalesce identical `Fetch`; reject `Background` class first |
| Input → Reader | bounded mpsc, priority lane | 64 | never expected; overflow is a bug, logged |
| Reader → Speech | **single-slot "latest intent" + small FIFO for queued speech** | 1 + 32 | interrupting intent replaces slot; FIFO drops lowest-priority-oldest |

The Reader polls with a **biased select**: Input, then focus-app lane, then query replies, then background lane. Keypresses always overtake event backlog.

### 5.3 Ingress coalescing

The per-app ingress is a keyed, order-preserving map, `IndexMap<CoalesceKey, Slot>`, plus a ring of sequence numbers. The connector writes into it under a short mutex or a lock-free SPSC (bake-off B-2).

```rust
enum CoalesceKey {
    Focus,                                   // last-writer-wins, one slot per app
    Props(NodeKey),                          // OR the PropMask; value fetched lazily
    Children(NodeKey),                       // collapse; ancestor ChildrenChanged subsumes descendants (cheap check: depth-limited)
    Text(NodeKey),                           // TextDelta merge: adjacent inserts concat; insert+delete cancel; > 64 deltas => Replaced
    Caret(NodeKey),                          // last-writer-wins
    Selection(NodeKey),
    Announcement(Seq),                       // never coalesced, but capped (rate limit per politeness)
    Window(NodeKey), Document(NodeKey), Removed(NodeKey),
}
```

Rules:

- R-1 **Supersession.** `Removed(n)` deletes every pending slot for `n`. `Focus` keeps only the newest node, but the Reader still learns the sequence length, for statistics.
- R-2 **Ordering.** Coalescing is order-preserving for the first occurrence. A later event for the same key updates the slot in place. The Reader sees each node's events in causal order per app. There is no cross-app ordering guarantee, and none is needed.
- R-3 **Overload.** If distinct keys exceed the bound, or the app actor lags more than 50 ms behind its ingress, the app enters `Flooded`. In `Flooded`, the ingress keeps only `Focus`, `Caret` on the focused node, `Removed`, `Window`, and the `Announcement` cap. Everything else becomes a per-node dirty bit in a bitmap or `HashSet`. On drain, the app actor resyncs the dirty set lazily: only nodes the Reader asks for, plus the focus ancestry.
- R-4 **Cost.** Push is O(1) amortized and allocation-free in steady state (preallocated map). Target: sustained 100k pushes/s per app on one core, which is 10× the product goal.

### 5.4 Deadlines and the app health state machine

| Budget class | Default deadline | Used for |
|---|---|---|
| Interactive | 120 ms total per user action, split across calls | focus presentation, nav step, keypress-driven queries |
| Prefetch | 300 ms | ancestry, siblings, document chunks for browse mode |
| Background | 2 s | full subtree sync, `GetItems` |
| Startup | 5 s | first contact with a newly launched app |

The 15 ms goal is gorca-side processing, so IPC wait is excluded. But the Interactive deadline bounds what the user hears: if the reply misses it, the Reader speaks from the cache and marks the result `may_refine`.

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
| Slow | 3 consecutive timeouts | Hung | reject Prefetch/Background; Interactive served from cache only; announce "application not responding" once if focused |
| Slow | 10 s without timeout | Healthy | restore concurrency |
| Hung | probe tick (exp. backoff 250 ms → 4 s) | Hung / Healthy | one cheap probe (`GetRole` on app root) with Interactive deadline |
| Healthy, Slow | ingress over watermark | Flooded | see R-3 |
| Flooded | backlog drained | Healthy | lazy resync of dirty set |
| any | owner vanished / connector `Gone` | Gone | tombstone all nodes; Reader gets `AppLifecycle::Gone` |

**Invariant I-4.** A request is never issued to a `Hung` app except as a probe. Whatever the app does, the reader issues at most one probe at a time per app.

### 5.5 Failure isolation

- Every actor is a task under the supervisor. A panic is caught at the task boundary (`JoinError::is_panic`), and the actor is restarted with fresh state from the connector, i.e. a resync. The release profile keeps `panic = "unwind"` for this reason.
- If an app actor panics, the supervisor restarts only that app actor. Focus can be lost, and the Reader then re-queries the focused node.
- Connector decoding of untrusted input (atspi `ObjectRef` asserts, AccessKit `Tree` panics) is contained per message or per app scope (C-6). A crash loop in one scope quarantines that scope; the user hears "application not accessible" once, and every other app keeps working.
- A Reader panic is fatal to user experience. The Reader is restarted, its state machines reset to `NoFocus`, and the error is spoken ("gorca restarted"). The Reader has no IPC and no extension code inside it, so its panic surface is small, which keeps this rare.
- The **watchdog** is an OS thread outside tokio. It monitors Reader and Input heartbeats; the heartbeat is the time since the last processed message while the queue is non-empty. A stall over 250 ms is logged with a stack via `tokio-console` or `backtrace`. A stall over 2 s triggers a supervised restart. Adapted from NVDA's `watchdog.py`.

## 6. Main state machines

All of them are `enum`s handled with exhaustive `match`, with no wildcard arm, and are unit-tested as pure transition functions `fn step(self, Msg) -> (Self, Effects)`.

### 6.1 Focus

```rust
enum FocusState {
    NoFocus,
    Settling { candidate: NodeKey, epoch: Epoch, until: Instant },   // debounce flapping focus (menus, GTK popovers)
    Focused  { node: NodeKey, epoch: Epoch, presented: bool },
    Orphaned { last: NodeKey, epoch: Epoch },                       // focused node removed / app gone; recover via window focus
}
```

| From | Msg | To | Effects |
|---|---|---|---|
| NoFocus / Focused / Orphaned | `FocusChanged(n)` | Settling{n, epoch+1, now+δ} | **cancel speech with epoch < new** (immediately, not after δ) |
| Settling{c} | `FocusChanged(n)` | Settling{n, epoch+1, now+δ} | cancel |
| Settling{c} | timer δ | Focused{c, presented:false} | query `Fetch(c, PRESENT_MASK)` + ancestry diff (Interactive) |
| Focused{n,!presented} | reply / cache hit | Focused{n, presented:true} | SpeechIntent(Interrupt, epoch) |
| Focused{n} | `NodeRemoved(n)` / app Gone | Orphaned | query active window's focus (Interactive) |
| Orphaned | `FocusChanged(n)` | Settling | |
| any | `Hung` for focus app | unchanged | speak "not responding" once per Hung episode |

δ defaults to 0 ms, which means presenting immediately. It rises to 30 ms only for roles known to flap: menu, menu item, popover. This is an extension-tunable policy. Measuring δ is part of metric M-6.

**Epoch.** A `u64` that increases on every accepted focus change, and on every mode change or explicit user navigation. Every speech intent and every query carries the epoch it was created in. This is the single cancellation primitive.

### 6.2 Navigation mode

```rust
enum Mode {
    Focus,                                              // follow system focus; keys pass through
    Browse  { doc: NodeKey, cursor: DocCursor, sticky: bool }, // virtual cursor over document model
    Review  { origin: NodeKey, cursor: ReviewCursor },  // flat/object review; terminal review lives here
    PassThrough { scope: PassScope },                   // OneKey | UntilToggle
    InputHelp,                                          // "learn mode": keys described, not executed
}
```

The transitions come from user commands, from focus entering or leaving a `DOCUMENT` subtree, and from editable focus inside a document. They mirror NVDA's auto focus mode. Extensions may veto or force auto-switching, for example for the Firefox editable rules. Full table in `docs/design/modes.md` (to be written; tracked as an open item).

The **document model** for browse mode is a core component, `gorca-doc`: a linearized, chunked buffer of text and control fields built from tree queries. It is connector-neutral. Chunks are built with `Prefetch` deadlines around the cursor and extended lazily. This is the riskiest component, open questions W-1 to W-4.

### 6.3 Speech queue

```rust
enum SpeechState {
    Idle,
    Speaking   { cur: UttId, prio: Prio, epoch: Epoch, sent_at: Instant },
    Cancelling { cur: UttId, next: Option<UttId>, deadline: Instant },  // waiting for synth to ack stop
    Paused     { resume: Option<UttId> },
    Disconnected { retry_at: Instant, pending: Option<UttId> },
}
enum Prio { Interrupt, Message, Text, Progress, Notification }   // cf. SSIP priorities, refs/speech-dispatcher/doc/ssip.texi:571
```

- At most **one** utterance is in flight to the synthesizer. The queue lives in gorca, not in speech-dispatcher. A cancel therefore stops only the current utterance; there is no remote queue to flush.
- `Interrupt` with epoch E drops every queued utterance with epoch < E, sends `CANCEL self`, and goes to `Cancelling`. The next utterance is sent **without waiting** for the cancel ack if the backend allows it (SSIP processes commands in order). Otherwise the state machine waits for the ack until its deadline (20 ms), then sends anyway and logs it.
- **Validity check at push time**, as in NVDA. Before an utterance is sent, `utt.epoch == reader.epoch`, or `utt.prio ∈ {Notification}` with policy `survives_focus_change`.
- `Disconnected`: exponential reconnect. Queued intents older than 2 s are discarded rather than spoken late.

## 7. Output pipeline

```
NodeEvent / KeyIntent ─► Reader: Presenter (role-based generators + extension overrides)
                          produces Utterance { segments: [Name|Role|State|Value|Text|Pause|Earcon], lang, epoch, prio, may_refine }
                       ─► Speech actor: verbalize (punctuation level, symbol tables, pronunciation dict) ─► SpeechBackend
                       ─► Braille actor: BrailleRegion list ─► BrailleBackend (BrlAPI)
```

```rust
pub trait SpeechBackend: Send {
    fn speak(&mut self, u: &Verbalized, marks: bool) -> BoxFuture<'_, Result<(), SpeechErr>>; // returns when *accepted*, not spoken
    fn cancel(&mut self) -> BoxFuture<'_, Result<(), SpeechErr>>;                             // deadline 20 ms
    fn events(&mut self) -> BoxStream<'_, SpeechEvent>;   // Started, IndexMark(u32), Ended, Cancelled
    fn set_params(&mut self, p: &VoiceParams) -> BoxFuture<'_, Result<(), SpeechErr>>;
}
```

- Presentation is split into **structure** (Reader) and **verbalization** (Speech actor). Generators produce segments, not strings, so braille and speech share them. Unlike Orca's generator chains, the Reader's generators read only snapshot data that is already cached; a missing field yields a `may_refine` utterance and a query.
- `may_refine`: when the reply arrives, the refined utterance replaces the in-flight one only if (a) the epoch is unchanged and (b) fewer than 150 ms of audio have been played. After that it is dropped. The exact rule is measured in M-6.
- Backends: `ssip` (speech-dispatcher, the default) and an `espeak-direct` candidate. Choosing between them is bake-off B-3.
- Braille: BrlAPI through brltty. Braille follows the same epoch, and a new focus region replaces the old one without a queue. This is lower risk, and braille-specific design will get its own ADR later.

## 8. Extension model

### 8.1 Tiers

| Tier | Runs as | Who writes it | Isolation | What it can touch |
|---|---|---|---|---|
| **Core policy** | Rust in `gorca-core` | core team | none (it *is* the core) | everything; must be connector- and app-neutral |
| **Extension (WASM)** | wasmtime component, one `Store` per (extension, app) | first party + third party | memory-safe sandbox, fuel/epoch interruption, memory limit, trap → instance reset | only the WIT world below |
| **Extension (native first-party)** — *conditional on bake-off B-4* | same Rust trait, linked in, run inside the app actor | first party only | panic caught at app-actor boundary; **no hang protection** | same API as WASM (no extra powers) |

### 8.2 What an extension can do (WIT world sketch)

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
  get-node: func(key: node-key) -> option<node>;                        // cache only, never blocks
  fetch: func(key: node-key, mask: u32, b: budget) -> result<node, query-error>; // async via component-model async or poll; deadline enforced by host
  text: func(key: node-key, start: u32, end: u32, b: budget) -> result<string, query-error>;
  speak: func(segments: list<segment>, prio: prio);                    // tagged with current epoch by host
  set-setting: func(k: string, v: string);                              // own namespace only
  log: func(level: u8, msg: string);
}

interface hooks {
  use types.{node};
  /// Event triage: may suppress, rewrite to another core event, or request focus re-evaluation.
  on-event: func(ev: event) -> event-verdict;
  /// Presentation override for a node in a context; returning none = use core generator.
  present: func(n: node, ctx: present-ctx) -> option<list<segment>>;
  /// Structural navigation predicate / custom nav units for browse and review modes.
  nav-units: func() -> list<nav-unit>;
  nav-match: func(unit: u32, n: node) -> bool;
  /// Commands bound in the extension's keymap.
  command: func(id: u32) -> command-result;
}

world extension {
  import host;
  export hooks;
}
```

The **manifest** declares a matcher, `app_name` / `toolkit` / `connector` / `document-role` globs, the hooks it implements, its keymap, and capabilities. The capabilities are `speak`, `fetch:prefetch`, `fetch:background` and `settings`. There is no raw IPC, no file system except a settings KV, and no network.

**Limits.** A hook call has a fuel or epoch budget of 2 ms CPU for `on-event` and `present`, and 20 ms for `command`. Memory is capped at 32 MiB per instance. A trap or limit exceeded means the instance is reset; three resets in 60 s disable the extension for that app, and the user is told once. The core continues with default behaviour.

**Ordering.** Several extensions can match one app. The order is: app-specific, then toolkit-specific, then connector-specific. `present` uses the first `Some`; `on-event` verdicts compose left to right.

### 8.3 Planned first-party extensions

`ext-gnome-shell`, `ext-vte` (terminal: text-change diffing into "new output", prompt detection, review integration), `ext-firefox` (browse-mode quirks, live-region tuning, editable heuristics) and `ext-chromium`. The tree and text plumbing they rely on lives in the core, in `gorca-doc` and the text-change coalescer. **Rule:** if two extensions need the same workaround, it moves into a connector, as normalization, or into the core, as a generic policy.

## 9. Workspace layout

```
crates/
  gorca-model/            # NodeKey, Role, StateSet, NodeSnapshot, NodeEvent, Utterance, Segment. no tokio, no I/O.
  gorca-connector-api/    # Connector, AppPort, BackendRequest/Op/Error, ingress coalescer
  gorca-core/             # app actors, Reader, state machines, presenter, supervisor; depends on model + connector-api + output-api + ext-api
  gorca-doc/              # document model for browse mode (connector-neutral)
  gorca-output-api/       # SpeechBackend, BrailleBackend, Verbalized
  gorca-speech-ssip/      # SSIP backend (ssip-client-async or own)
  gorca-speech-espeak/    # bake-off candidate (B-3)
  gorca-braille-brlapi/
  gorca-input-api/        # KeySource trait, KeyEvent, grab/consume semantics
  gorca-input-gnome/      # org.a11y.atspi KeyboardMonitor / Mutter path (see P-1)
  gorca-input-libei/      # fallback
  gorca-connector-atspi/  # atspi + zbus. the only crate allowed to depend on atspi/zbus (besides speech/input crates that use D-Bus)
  gorca-connector-newton/ # accesskit_consumer + Newton transport
  gorca-ext-api/          # WIT files + Rust trait mirror + bindings
  gorca-ext-host/         # wasmtime embedding, limits, manifest loading
  gorca-daemon/           # binary: config, wiring, supervisor, systemd user unit
  gorca-testkit/          # fake connector, scripted apps, virtual clock, conformance suite, storm generator
extensions/
  ext-gnome-shell/ ext-vte/ ext-firefox/ ext-chromium/
tools/
  (Go/Python tooling; accessibility inspectors, trace recorders)
```

**Dependency rules** are enforced in CI by a `cargo metadata` check:

- `gorca-core`, `gorca-model`, `gorca-doc` and `gorca-connector-api` must not depend, even transitively, on `atspi`, `zbus`, `accesskit*`, `wayland-*` or `wasmtime`.
- `gorca-core` may not depend on any `gorca-connector-*` or `gorca-speech-*`; only `gorca-daemon` wires them together.
- Extensions depend only on `gorca-ext-api`.

## 10. Alternatives considered

| Alternative | Why not |
|---|---|
| **Orca-style single loop with synchronous IPC** | The root cause of Orca's freezes: one hung app delays everything by its timeout, and reentrancy creates order bugs (`refs/at-spi2-core/dbind/dbind.c:66-130`). |
| **Single global actor with async handlers (Odilia-style)** | Simpler, but an awaited IPC inside a handler serializes all apps behind the slowest one. Odilia's per-event spawned tasks also lose ordering. See the report, §2. |
| **Live proxies as the object model (AT-SPI style `Accessible` objects)** | Every property read becomes IPC, which makes latency impossible to bound. It also does not fit Newton's push model. |
| **Connector as an in-core callback trait, with the core calling `connector.get_name(node).await`** | It couples core latency to connector internals. It cannot be moved out of process later, and the per-app health state machine would end up spread across connectors. |
| **Eager full mirror of every app (Newton-style for AT-SPI too)** | Startup and memory cost for large apps, such as a Firefox page with 50k+ nodes, and D-Bus traffic storms. It is tested anyway as one arm of bake-off B-1. |
| **Speech queue kept in speech-dispatcher, using SSIP priorities only** | Cancellation semantics are remote and coarse. `important` cannot be cancelled by `message` (`ssip.texi:571-593`). We lose epoch validity checks. |
| **Lua or Python scripting for extensions** | No hard CPU or memory limits, and a GC can pause. WASM gives sandboxing, limits and a typed interface (WIT) that we can version. |
| **Out-of-process extensions over IPC** | Strongest isolation, but each hook costs a context switch (tens of µs). Too slow for `present` on every focus change. Kept as an option for untrusted third-party extensions later. |

## 11. Consequences

**Positive**

- A hung or flooding app can affect only its own actor and its own utterances. The Reader stays responsive by construction: it does no awaits, polls input first, and uses bounded queues.
- Newton slots in as a second connector. The Newton connector's job is mostly a diff into `NodeEvent`, plus local query answering.
- The state machines are pure, so they can be property-tested and replayed from recorded traces. Recording `(Msg, Instant)` streams gives deterministic bug reproduction.
- Extensions cannot block the reader.

**Negative / costs**

- Message passing makes the code more verbose than direct calls. "Ask the app, then continue" becomes a two-step state machine in the Reader. Mitigation: a small `Pending<ReqId, Continuation>` helper, and keeping continuations as enum variants.
- Stale-cache presentation (`may_refine`) can occasionally speak an outdated name and then correct it. Metric M-6 must show this is rare.
- Browse mode over out-of-process queries may never match NVDA's in-process buffers on huge pages without server-side help, such as AT-SPI `Collection` or Firefox's cache. See open question W-1.
- WASM adds build complexity (`cargo component`, WIT versioning) and a per-call overhead that has to be measured (B-4).

## 12. Metrics to validate

| ID | Metric | Target | How measured |
|---|---|---|---|
| M-1 | keypress (input actor receipt) → first `SPEAK` byte written to SSIP socket, cache hit | p99 < 15 ms, p50 < 3 ms | perf-engineer harness with fake connector + real Reader; tracing spans |
| M-2 | same with cache miss, app replying in 5 ms | p99 < 15 ms gorca-side (IPC excluded via span accounting) | fake AT-SPI app on private bus |
| M-3 | sustained ingest | 10k ev/s for 60 s across 5 apps: no unbounded growth, Reader lag < 20 ms; 100k ev/s burst on 1 app: other apps' M-1 unaffected (Δp99 < 1 ms) | storm generator in `gorca-testkit` |
| M-4 | hung app isolation | focused app sleeps forever: speech of "not responding" < 500 ms after first keypress; M-1 in another app unaffected | fake app that stops replying |
| M-5 | cancellation latency | new focus → `CANCEL` written to backend < 2 ms gorca-side; stale utterance audible after new focus: 0 cases in 10k randomized trials | trace assertions |
| M-6 | refine/debounce quality | `may_refine` corrections < 1% of focus presentations in scripted GNOME sessions; no double-speak of the same focus | recorded session replays |
| M-7 | extension isolation | WASM trap or infinite loop in `present`: core recovers < 50 ms; hook overhead p99 < 100 µs for `present` | ext-host bench |
| M-8 | memory | < 150 MB RSS with Firefox (10 tabs) + Terminal + Shell | smem in CI-like session |
| M-9 | browse-mode build | Firefox page of 10k nodes: first line speakable < 150 ms after load-complete; full buffer < 2 s, background | web harness (web-a11y-engineer) |
| M-10 | Newton parity | same scripted GTK scenario through AT-SPI and Newton connectors yields identical utterance traces (modulo documented differences) | dual-connector replay |

## 13. Bake-off candidates

| ID | Decision | Arms | Judge on |
|---|---|---|---|
| **B-1** | AT-SPI mirror cache strategy | (a) lazy on-demand + invalidation; (b) eager per-window `GetItems` mirror; (c) hybrid: eager for focus window ancestry/siblings + lazy elsewhere | M-1/M-2 hit rate, D-Bus msgs/s, RSS, M-9 |
| **B-2** | ingress coalescer implementation | (a) `Mutex<IndexMap>`; (b) lock-free MPSC ring + generation-tagged lazy dedupe in actor; (c) sharded per-node-hash | M-3 throughput, p99 push cost, CPU |
| **B-3** | speech backend | (a) SSIP via speech-dispatcher; (b) in-process espeak-ng → PipeWire | key→audio latency (measured with loopback capture), cancel→silence latency, robustness |
| **B-4** | first-party extension execution | (a) WASM-only; (b) dual-target native + WASM | M-7 overhead, ext-vte under `cat bigfile` storm, ext-firefox browse build time |
| B-5 *(measure, no full bake-off)* | Reader thread isolation | dedicated current-thread runtime vs shared multi-thread | M-1 under M-3 load |

## 14. Open questions (owners)

**linux-platform-engineer**

- P-1: The exact contract of `org.a11y.atspi.KeyboardMonitor` on GNOME 48 / Mutter. Can it **consume** keys for gorca commands, what is its latency, and how does it behave when gorca is slow? Is there a deadline after which the key passes through? Compare registryd's 3 s (`refs/at-spi2-core/atspi/atspi-misc.c:1942-1949`).
- P-2: Does AT-SPI on GNOME 48 support event registration filtered per app or per event type on the bus (match rules), so that flooding apps can be throttled on the broker side?
- P-3: dbus-broker behaviour when gorca's receive queue is slow: quota per peer, and whether messages are dropped or we are disconnected. This sets the ingress design limits.
- P-4: Reliability of `GetItems` for GTK4, Firefox and Chromium today, and whether `AddAccessible` / `RemoveAccessible` are emitted consistently. B-1 depends on this.
- P-5: SSIP `CANCEL self` latency to silence with espeak-ng, and index-mark support. B-3 depends on this.
- P-6: How does keyboard monitoring on GNOME 48 compare with Newton's Manager `KeyEvent` / `SetKeyGrabs` design? Is the Mutter a11y key path on GNOME 48 access-controlled?

**web-a11y-engineer**

- W-1: Can Firefox's AT-SPI exposure (`Collection`, `Hypertext`, text attributes) build a browse-mode buffer within M-9, or is a per-browser fast path needed? If one is needed, where does it go: a connector feature or `ext-firefox`?
- W-2: Firefox and Chromium event patterns on page load (storms of `children-changed` and `text-changed`). Which coalescing keys are safe?
- W-3: Live-region semantics and the attributes exposed (`container-live`, `container-relevant`, `container-atomic`). Can they map onto the `Announcement` event without app-specific code?
- W-4: Identity stability of web nodes across DOM mutations, which constrains invariant I-1.

**architect / Newton (no active owner yet).** The transport is known: report §3.1. The open points:

- N-1: **Security model.** Newton's Manager has no sender checks. `KeyEvent` is a broadcast signal, which makes it a keylogger for every session-bus client, and anyone can call `GrabKeyboard`. gorca must not depend on Newton input until there is an access-controlled channel (a portal or a private bus). Who drives that upstream?
- N-2: **Wire format.** JSON pinned to AccessKit 0.16; current is 0.25 with `deny_unknown_fields`. Do we target a revived Newton on current AccessKit, or ship a 0.16 shim for the prototype stack?
- N-3: **`LocalId` scheme.** `(surface << 32) | node_low32` works for GTK's 32-bit counters. Current AccessKit multi-tree support needs `(tree_id, node_id)` interning. Text-run ids are unstable (`refs/newton/gtk/gtk/a11y/gtkaccesskitcontext.c:988-993`), so cursors anchor to the container plus an offset, never to a run.
- N-4: **App identity.** No pid or app-id in Newton, so dual exposure cannot be deduplicated automatically, and per-app extensions cannot match by app name. This needs a protocol addition, for example Mutter exposing the client's app-id or pid from Wayland client credentials.
- N-5: **Text units.** AccessKit offsets are USV- or UTF-16-based (`refs/accesskit/accesskit_consumer/src/text.rs`); AT-SPI offsets are characters. `TextQuery` uses Unicode scalar offsets in the core, and each connector converts.

## 15. Decisions requiring sign-off

1. Message-based connector boundary with a mandatory per-request deadline (§4), instead of an async callback trait.
2. A non-awaiting Reader actor with epoch-based cancellation (§5.1, §6).
3. gorca owns the speech queue, with one utterance in flight to the synthesizer (§6.3).
4. Extensions: WASM by default; native first-party only if B-4 shows WASM cannot meet M-7 or M-9.
5. Dual-exposure policy: Newton wins over AT-SPI for the same app (§4, C-5).
