# ADR-0002: Multimodal perception layer — feature store, vector index, map surface, `gorca-perceive` sidecar

- **Status:** Accepted (2026-10-12), with one amendment by the owner: terminal content encoding is user-toggleable and off by default, and obvious secrets are always filtered (§6.2 X-7, X-11). These rules govern only what the perception layer embeds and persists; speech, braille and navigation output are never filtered (PR-0). All §17 sign-off items are approved. **§10 (screen-pixel acquisition) stays open** while linux-platform-engineer evaluates a GNOME Shell extension against a Mutter/Shell patch.
- **Date:** 2026-10-12
- **Deciders:** project owner
- **Extends:** ADR-0001 (§3 concurrency model, §6 actors and backpressure, §7.2 Mode, §11 workspace layout, §14 metrics). It does not change any accepted ADR-0001 decision.
- **Related:**
  - `docs/reports/2026-10-12-multimodal-vectors-som.md` (MM): measured encoders, SOM, ANN crossovers, privacy constraints. Code in `research/multimodal/`.
  - `docs/reports/2026-10-12-math-web-graph-model.md` (content dedupe: 100k nodes collapse to about 1.7k unique skeletons).
  - `docs/reports/2026-10-12-web-source-layer.md` (element screenshots through the browser source layer).

## 0. Changelog

| Date | Change |
|---|---|
| 2026-10-12 | Draft (Proposed). |
| 2026-10-12 | **Accepted with amendment.** All §17 items approved. X-7 changed: terminal content encoding is a user toggle, default off; new X-11 always filters obvious secrets (password prompts, tokens, private keys), including when the toggle is on; `Exclusion::SecretPattern` added (§5.2, §6.2). §10 (pixel capture) remains open. |
| 2026-10-12 | Secret-filter deferral reverted; it was a misunderstanding about reading vs. embedding. X-1…X-11 keep their approved defaults. New PR-0 states that speech, braille and navigation output are never filtered. |

## 1. Context

gorca gains a perception layer: vectors for accessible nodes (text first, pixels later), search over them, and a navigable 2-D overview. The owner approved the scheme on 2026-10-12. This ADR fixes the interfaces, process boundary, wire protocol, privacy model and budgets so that implementation can start.

Forces:

| Force | Source | Consequence |
|---|---|---|
| The CLAUDE.md latency goal: p99 < 15 ms key → speech, gorca-side | CLAUDE.md, ADR-0001 M-1 | Perception never runs on the key → speech path. Every result arrives as a later, epoch-tagged message that may be dropped. |
| Never block, deadline on every IPC call | ADR-0001 R2, D-4, C-2 | The sidecar link follows the connector contract: a mandatory deadline per request, exactly one terminal reply or a dropped sender. |
| Crash isolation | CLAUDE.md | ONNX Runtime (C++), Tesseract (C++) and model parsing are large native attack and crash surfaces. They live in another process. |
| Model size and compute: 50–400 MB models, SIMD, threads | MM §4 | Not WASM: ORT and candle do not run well in wasmtime (MM §0a). A native process is required. |
| Re-renders and duplicates dominate trees | MM §0b, graph-model report | The store is keyed by content, not by node identity. |
| Embeddings are plaintext-equivalent (Morris et al., 2023, "Text Embeddings Reveal (Almost) As Much As Text"; to be re-verified, MM §11) | MM §7 | Vectors carry the same privacy class as the text they encode. |
| Exact search is fast enough up to about 1.5×10⁵ × 384 f32 on one core | MM §7 | Exact scan is the only index for now. ANN sits behind the same trait. |
| SOM with anchored refit and hysteresis is the only measured layout that is high-quality, stable and fast at once | MM §6.3 | It is the default `MapLayout`, behind a flag, until a user study passes. |
| Pixels: `org.gnome.Shell.Screenshot` rejects us; Mutter ScreenCast works but shows an indicator | §10 | Pixel acquisition is an open platform question (§10). Phase 1 needs no pixels. |

## 2. Decision (summary)

1. **Three generic core pieces**, all connector-neutral and all off the hot path:
   - **Feature store** (`gorca-features`): content-keyed (§7), bound to the mirror lifecycle by reference counts, with a **privacy policy evaluated in core before any key is computed** (§6). An input that is excluded never gets a key, so it can never be requested, cached or persisted.
   - **`VectorIndex`** (`gorca-vector`): exact f32 SIMD scan. An ANN implementation (HNSW f16) may be added behind the same trait only for indexes above about 1.5×10⁵ vectors (§5.3).
   - **`MapSurface`** (`gorca-map`): a navigable 2-D grid state machine. `SomLayout` (pure Rust, anchored incremental refit, sticky-cell hysteresis) is the default layout, behind the cargo feature `map` and the runtime setting `experimental.map_surface` (§8).
2. **Encoders run in `gorca-perceive`**, a separate native process hosting ONNX Runtime, with **no network access**. The sidecar enforces this itself (seccomp and Landlock, fail-closed) and the systemd unit enforces it again (§4.6). The sidecar is **stateless**: it keeps nothing but model weights and loaded vocabularies.
3. **Wire protocol:** a Unix stream socket, length-prefixed postcard frames, images passed as sealed memfds, client-chosen request ids, a per-request deadline, epoch-based cancellation and credit-based backpressure (§4).
4. **Pixel capture stays in the gorca daemon** behind a `PixelSource` trait. The sidecar never talks to D-Bus, PipeWire or the compositor (§4.6, §10). *Proposed.*
5. **First feature: semantic find** (text only, e5-small int8), with lexical matching that always works, even without the sidecar (§9.1). **Second: the unlabeled-control naming cascade**: hints, then label memory, then OCR, then SigLIP with abstention (§9.2).
6. **Phased rollout** with exit criteria per phase (§13).

## 3. Crate placement (extends ADR-0001 §11)

### 3.1 New crates

```
crates/
  gorca-perceive-api/     # neutral. Modality, EncoderId, EncoderInfo, PerceiveClass, PerceiveReq/Reply,
                          # PerceiveClient trait (never blocks, like SpeechClient), PerceiveHealth,
                          # PixelSource trait + CaptureReq/Frame (pixel acquisition is abstract here)
  gorca-perceive-proto/   # neutral, tokio-free, depends on no gorca crate. Wire messages (§4.3),
                          # framing codec, memfd image descriptor, PROTO_VERSION. Shared by both ends.
  gorca-perceive-client/  # concrete. PerceiveClient over the Unix socket: writer/reader tasks, credits,
                          # dedupe, deadlines, health SM (§4.5). Wired only by gorca-daemon.
  gorca-perceive/         # binary `gorca-perceive` (the sidecar). ort + tokenizers; later leptess.
                          # Self-sandbox (§4.6). Depends on gorca-perceive-proto only.
  gorca-vector/           # neutral, tokio-free. VectorIndex trait, ExactF32 (SIMD), f16 helpers;
                          # feature "ann" adds HnswF16 later (§5.3).
  gorca-map/              # neutral, tokio-free. MapLayout trait, SomLayout (feature "som"), MapSnapshot,
                          # precomputed step tables, cell labeller (TF-IDF), MapState SM (§8)
  gorca-features/         # neutral. ContentKey derivation, PrivacyPolicy + default policy, Feature
                          # actor (store owner), NodeBindings, label memory, encrypted persistence
                          # behind a KeyProvider trait (§6.4)
  gorca-models/           # binary `gorca-models`: explicit, user-invoked model fetch + sha256 verify.
                          # The only crate allowed an HTTP client. Never run by gorca or the sidecar.
```

**Not scaffolded now:** `gorca-capture-<backend>` (one crate per pixel backend, created after the §10 decision), `HnswF16` (Phase 5), the OCR and SigLIP code paths in `gorca-perceive` (Phase 3).

Placement of the rest:

- **Semantic find** (`FindJob`, hybrid ranking, `Find` mode) lives in `gorca-core`. It runs on `cpu`/`workers`, never inline on the Reader (§5.1).
- The **Secret Service `KeyProvider`** (D-Bus `org.freedesktop.secrets`) lives in `gorca-dbus`, which may use zbus. `gorca-daemon` wires it.
- **testkit** gains a `FakePerceive` (in-process `PerceiveClient` with scripted latency, hangs, crashes and canary detection) and a socketpair mode that runs the real sidecar binary (§4.1).

### 3.2 Dependency rules (additions to ADR-0001 §11; enforced by `cargo xtask deps`)

Rules 1–7 remain unchanged. The constant lists in `xtask/src/deps.rs` grow as follows:

| List | Added crates |
|---|---|
| `NEUTRAL` (rule 1) | `gorca-perceive-api`, `gorca-perceive-proto`, `gorca-vector`, `gorca-map`, `gorca-features` |
| `TOKIO_FREE` (rule 4) | `gorca-perceive-proto`, `gorca-vector`, `gorca-map` |
| `CONCRETE` (rule 3) | `gorca-perceive-client`, every `gorca-capture-*` except `gorca-capture-api` (if it is ever split out) |
| `is_transport` (rule 1) | `pipewire*`, `libspa*`, `ort*`, `onnxruntime*`, `tokenizers`, `leptess`, `tesseract*` |

New rules:

8. **Native inference crates** (`ort*`, `onnxruntime*`, `tokenizers`, `leptess`, `tesseract*`, `candle*`) may be depended on only by `gorca-perceive`.
9. **`gorca-perceive` depends on no workspace crate except `gorca-perceive-proto`.** No workspace crate may depend on `gorca-perceive` (it is a binary).
10. **No HTTP or TLS client crate** (`reqwest`, `hyper`, `ureq`, `curl*`, `isahc`, `attohttpc`, `rustls*`, `native-tls`, `openssl*`) appears in the production graph of any crate except `gorca-models`. In particular, `ort` is used with `default-features = false` and without its `fetch-models` and `download-binaries` features. This rule makes "no network" checkable at build time as well as at run time.
11. **`pipewire*`/`libspa*`** may be depended on only by `gorca-capture-*` backends. `zbus` is additionally allowed in those backends (rule 2 list grows when they are created).
12. `gorca-core` may depend on `gorca-map` only through the optional cargo feature `map`. Release builds enable it only after Phase 4 exit (§13).

`gorca-models` is a workspace member but is not wired into `gorca-daemon`. CI checks that `gorca-daemon`'s graph does not contain it.

## 4. Wire protocol: gorca ⇄ `gorca-perceive`

### 4.1 Transport and process lifecycle

- **Socket:** `AF_UNIX`, `SOCK_STREAM`, at `$XDG_RUNTIME_DIR/gorca/perceive.sock`. The directory is mode 0700 and the socket 0600.
- **Activation (proposed):** a systemd user socket unit `gorca-perceive.socket` (`ListenStream=%t/gorca/perceive.sock`, `SocketMode=0600`) activates `gorca-perceive.service`. The sidecar starts only when a feature first needs it, so a user who never uses perception pays nothing. The sidecar exits after 10 min idle (`--idle-exit=600s`).
- **Test and development mode:** `gorca-perceive --conn-fd=N` serves one already-connected socketpair end. testkit uses this; it needs no systemd.
- **Peer check:** on accept, `SO_PEERCRED` must give the sidecar's own uid, or the sidecar closes the connection. Same-uid processes are inside the trust boundary, as on the D-Bus session bus.
- **One active client:** a new connection with a valid `Hello` replaces the previous one. A restarted gorca therefore reconnects immediately, without waiting for the old socket to time out.
- **Why not D-Bus:** the sidecar would need session-bus access, and the session bus reaches services that do have network access (portals, browsers, NetworkManager). That would be an exfiltration channel. A private socket with no `connect()` capability (§4.6) closes it.

### 4.2 Framing

```
frame   := len:u32le kind:u8 body[len-1]      // len counts kind + body; len <= MAX_FRAME (256 KiB)
body    := postcard(Message)                   // gorca-perceive-proto::Message, versioned enum
fds     := SCM_RIGHTS on the same sendmsg      // only for frames whose body references FdSlot(i)
```

- **Images are never inline.** An image is a `memfd` sealed with `F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_WRITE | F_SEAL_SEAL` before sending, described by `ImageRef { fd: FdSlot, w, h, stride, format: Rgba8 | Bgra8 | Gray8, scale_milli }`. Seals make the size and content immutable, so the sidecar can `mmap` it without validation races. Pixels stay in RAM: memfds are anonymous and are closed after the reply.
- A frame longer than `MAX_FRAME`, an unknown `kind`, a postcard error or an fd count mismatch is a **protocol error**. The receiver closes the connection; neither side tries to resynchronise. The client then goes to `Down` (§4.5).
- **Handshake:** the client sends `Hello { proto: (major, minor), client_build }`. The sidecar answers `HelloAck { proto, encoders: Vec<EncoderInfo>, lanes: LaneCaps }`. If the major versions differ, the connection is closed and the client goes to `Disabled { VersionMismatch }`. Minor versions add optional fields only.

### 4.3 Messages

```rust
// crate gorca-perceive-proto. All ids are client-chosen. Sidecar never initiates a request.
pub struct ReqId(pub u64);              // monotonic per connection, never reused
pub struct WireEpoch(pub u64);          // gorca EpochCell value at creation (ADR-0001 E-1)
pub enum Lane { Interactive, Bulk }     // §4.4
pub struct ReqHeader { pub id: ReqId, pub epoch: WireEpoch, pub lane: Lane, pub budget_us: u32 }

pub enum ClientMsg {
    Hello      { proto: (u16, u16), client_build: u32 },
    Encode     { h: ReqHeader, encoder: EncoderRef, inputs: Vec<EncodeInput> },     // batch <= LaneCaps.max_batch
    Ocr        { h: ReqHeader, image: ImageRef, langs: SmallVec<[Lang; 2]>, mode: OcrMode },
    NameIcon   { h: ReqHeader, image: ImageRef, vocab: VocabId, top_k: u8 },
    Fingerprint{ h: ReqHeader, image: ImageRef },                                     // ink-mask pHash (MM §10)
    LoadVocab  { h: ReqHeader, vocab: VocabId, locale: Lang, labels: Vec<String> },   // closed UI vocabulary
    Cancel     { id: ReqId },
    CancelBefore { epoch: WireEpoch, lane: Lane },     // drop every queued request with epoch < e on that lane
    Ping       { nonce: u64 },
}
pub enum EncodeInput { Query(String), Passage(String), Image(ImageRef) }   // e5 "query: "/"passage: " prefixes added by the sidecar

pub enum SidecarMsg {
    HelloAck     { proto: (u16, u16), encoders: Vec<EncoderInfo>, lanes: LaneCaps },
    Vectors      { id: ReqId, dim: u16, rows: Vec<f32> },            // row-major, L2-normalised, one row per input
    OcrLines     { id: ReqId, lines: Vec<OcrLine> },                  // { text, bbox, conf }
    IconScores   { id: ReqId, top: SmallVec<[(u16, f32); 8]>, margin: f32 },   // abstention is decided in core
    Fingerprint  { id: ReqId, phash: u64, ink: Box<[f32; 1024]> },    // 32×32 zero-mean ink mask
    VocabLoaded  { id: ReqId },
    Error        { id: ReqId, code: ErrCode },
    Pong         { nonce: u64, queued: [u16; 2], inflight: [u16; 2], rss_kib: u32 },
}
pub enum ErrCode { Cancelled, DeadlineExceeded, Overloaded, ModelUnavailable, BadInput, Internal }
pub struct EncoderInfo { pub name: String, pub version: u32, pub dim: u16, pub modality: u8, pub model_sha256: [u8; 32], pub loaded: bool }
pub struct LaneCaps { pub interactive_credits: u8, pub bulk_credits: u8, pub max_batch: u16, pub max_frame: u32 }
```

Invariants:

- **W-1 (C-2 analogue):** every request gets exactly one terminal message (`Vectors`, `OcrLines`, `IconScores`, `Fingerprint`, `VocabLoaded` or `Error`), unless the connection dies. Replies may come out of order.
- **W-2:** the sidecar keeps no input-derived state after the terminal reply. There is no content cache, no log of inputs and no file write. Only model weights and `LoadVocab` vocabularies (UI words, not user content) persist for the life of the connection.
- **W-3:** abstention thresholds, label policy and ranking live in core. The sidecar returns scores only, so policy is testable without models.
- **W-4:** `Fingerprint` (ink mask, MM §10) runs in the sidecar, so untrusted pixel buffers are never parsed in the gorca process.

### 4.4 Deadlines and cancellation (ADR-0001 R2)

- **The client is the deadline authority.** Every `PerceiveReq` has an absolute `deadline: Instant` in gorca. The client arms a timer. When it fires, the waiting oneshot resolves to `Timeout`, the client sends `Cancel{id}`, and a late reply is discarded by id and counted. This is C-2 with a 5 ms grace.
- **The sidecar gets a relative budget** (`budget_us`, the remaining time at send). It computes its own local deadline on receipt, so the two processes never compare clocks. A request whose local deadline has passed before it starts is answered `DeadlineExceeded` without running. Long runs are aborted mid-inference through ORT's run-termination flag (`RunOptions` terminate; verify in the `ort` crate) and Tesseract's progress-monitor cancel callback.
- **Epoch supersession:** a new find query or a focus change advances the epoch (ADR-0001 E-1). The client sends `CancelBefore{epoch, Interactive}`. The sidecar drops queued interactive work from older epochs in O(queue) and answers each one `Cancelled`. Bulk work is not epoch-scoped, because it fills a shared cache that stays useful.
- **Perception classes and default deadlines:**

| `PerceiveClass` | Lane | Deadline | Used for |
|---|---|---|---|
| `Query` | Interactive | 50 ms | encoding a find query; `CancelBefore` on every new query |
| `Name` | Interactive | 500 ms | the visual naming cascade for the focused control (Phase 3) |
| `Scope` | Bulk | 2 s per batch | encoding the active scope (focused window or document) |
| `Background` | Bulk | 10 s per batch | warming other scopes; first to be shed |

### 4.5 Backpressure and health

**Credits.** `HelloAck.lanes` grants `interactive_credits` (default 4) and `bulk_credits` (default 2 batches of up to `max_batch` = 32 inputs). The client sends a request only when it holds a credit, and gets the credit back with the terminal reply. The sidecar's own queues are therefore bounded by construction. If it is still overloaded (CPU starvation), it answers `Overloaded`.

**Client-side pending set** (owned by the `perceive-io` task, which is single-writer):

| Queue | Bound | Dedupe | When full |
|---|---|---|---|
| Interactive | 8 | by `(ContentKey, EncoderId)` | drop the oldest-epoch entry; it resolves `Superseded` |
| Bulk | 4096 keys | by `(ContentKey, EncoderId)`; a second requester joins the existing entry | drop the oldest `Background` entry, then the oldest `Scope` entry. A dropped key goes back to `Absent` and is requested again on demand |

Batching: Bulk inputs are packed until `max_batch` or 2 ms have passed, whichever comes first. Two requests for the same content key share one inference (refcount on the pending entry).

**Sidecar scheduling.** There are two worker threads on one ORT session (ORT allows concurrent `Run`). The interactive worker runs at normal priority. The bulk worker runs with `setpriority(+10)` and processes batches capped at about 8 ms of compute, so an interactive request waits behind at most one bulk batch on a shared core. The intra-op thread counts are tuned by PM-2.

**Health state machine** (client side; the `PerceiveClient::health()` result):

```rust
enum PerceiveHealth {
    Disabled   { reason: DisabledReason },          // config off, models missing, VersionMismatch, user paused
    Connecting { attempt: u32, retry_at: Instant },
    Handshaking{ deadline: Instant },                // 2 s (+ model load, see PM-9)
    Ready      { caps: LaneCaps },
    Degraded   { caps: LaneCaps, since: Instant },   // >= 3 timeouts in 5 s
    Down       { retry_at: Instant, attempt: u32 },  // EOF, protocol error, handshake timeout
    Quarantined{ until: Instant },                   // >= 3 crashes (EOF with requests in flight) in 60 s
}
```

| From | Event | To | Effect |
|---|---|---|---|
| Disabled | config on and models present | Connecting | — |
| Connecting | connect ok | Handshaking | send `Hello` |
| Connecting | ENOENT/ECONNREFUSED | Down | backoff 250 ms → 8 s |
| Handshaking | `HelloAck` (major matches) | Ready | publish `EncoderInfo`s to the feature store; stale encoder versions become `Absent` |
| Handshaking | major mismatch | Disabled{VersionMismatch} | log once |
| Handshaking | deadline | Down | — |
| Ready | 3 timeouts in 5 s | Degraded | pause the `Background` class; halve bulk credits |
| Degraded | 10 s without timeout | Ready | restore credits |
| Ready, Degraded | EOF or protocol error | Down, or Quarantined if the crash rule matches | every in-flight request resolves `Unavailable` at once (no waiting for deadlines); pending entries go back to `Absent` |
| Down | backoff tick | Connecting | — |
| Quarantined | `until` (10 min) or the user's "retry perception" command | Connecting | — |
| any | user "pause perception" | Disabled{Paused} | `CancelBefore(u64::MAX)` on both lanes, then close |

**What the user notices when the sidecar is down or slow:**

- Nothing changes on the hot path. No speech, focus or navigation path waits on the sidecar; this is PM-1.
- **Semantic find** falls back to lexical ranking only (§9.1). The first time this happens in a session, gorca says "Semantic search unavailable, using text match" once.
- **Naming cascade:** the non-visual hints and the structural label-memory steps still work. The OCR and SigLIP steps are skipped silently.
- **Map surface:** existing maps stay navigable, because they are built from vectors already in the store. Cells without vectors fall back to structure and geometry blocks (MM §3).
- `Quarantined` is announced once, and only if the user invokes a perception feature while it lasts.

### 4.6 No-network enforcement and sandbox (proposed: both layers mandatory)

**Layer 1: the sidecar sandboxes itself. This is mandatory and fail-closed.** After the models load and before it accepts the first request, the sidecar:

1. Sets `PR_SET_NO_NEW_PRIVS` and `PR_SET_DUMPABLE=0`. Core dumps would contain pixels and text.
2. Applies a **Landlock** ruleset. The model directory is read-only and nothing is writable. Where the kernel ABI supports it (ABI ≥ 4 for TCP, ≥ 6 for abstract Unix sockets and signals), it denies TCP bind and connect and scopes abstract sockets and signals.
3. Installs a **seccomp** filter that returns `EPERM` for `socket`, `connect`, `bind`, `listen`, `execve`, `execveat`, `ptrace`, `process_vm_readv`, `process_vm_writev`, `io_uring_setup`, `bpf` and `perf_event_open`. It keeps `accept4` on the inherited listener, plus `recvmsg` and `sendmsg` on accepted fds. Because `socket()` is denied, the sidecar cannot open a path to the network, to D-Bus or to PipeWire, whatever the libraries do.
4. Runs a **self-test**: `socket(AF_INET)` and `socket(AF_UNIX)` must both fail with `EPERM`. If either succeeds, or if any step above fails, the sidecar exits with status 78 (`EX_CONFIG`), and the client goes to `Disabled{SandboxUnavailable}`. **Perception never runs unsandboxed.**

**Layer 2: the systemd unit (defence in depth).**

```ini
[Service]
ExecStart=/usr/libexec/gorca/gorca-perceive --models=%h/.local/share/gorca/models --idle-exit=600s
PrivateNetwork=yes            # needs unprivileged user namespaces in the user manager; verify on Debian 13
RestrictAddressFamilies=AF_UNIX
IPAddressDeny=any             # may be a no-op in user units (BPF cgroup programs); verify
NoNewPrivileges=yes
LockPersonality=yes
MemoryDenyWriteExecute=yes    # verify that ORT's CPU EP does not JIT
ProtectSystem=strict
ProtectHome=read-only
PrivateTmp=yes
LimitCORE=0
MemoryMax=768M
CPUWeight=20
Nice=5
OOMScoreAdjust=500
Restart=on-failure
StartLimitIntervalSec=60
StartLimitBurst=3
```

Some unit directives may be ineffective in a user manager; that is open question O-3. Layer 1 alone is sufficient for the guarantee. Layer 2 adds resource control and a second barrier.

**Model provenance.** Models are fetched only by the explicit command `gorca-models fetch`, run by the user. Files are verified against SHA-256 values pinned in the source tree, and stored under `$XDG_DATA_HOME/gorca/models/` (default `~/.local/share/gorca/models/`). The sidecar refuses a model whose hash does not match its `EncoderInfo.model_sha256`. gorca never downloads anything on its own.

## 5. Core components

### 5.1 Actors and lanes (extends ADR-0001 §6)

| Actor / task | Count | Lane | Owns | Never does |
|---|---|---|---|---|
| **Feature actor** | 1 | workers | `FeatureStore` (single writer, PC-1): content-keyed entries, refcounts, LRU, label memory, persistence | presentation; inline encoding |
| **perceive-io** | 1 | workers | socket, credits, pending set, `PerceiveHealth` | policy |
| `FindJob`, `MapBuild`, `MapUpdate` | per request | cpu | nothing; returns epoch-tagged results | touch the socket |
| Reader | 1 | reader | `Mode::Find`, `Mode::Map`, nav cursors over `ArcSwap<MapSnapshot>` and `FindResults` | await perception; scan vectors |

| Edge | Channel | Bound | When full |
|---|---|---|---|
| app/doc actor → feature actor | binding batches (acquire/release per mirror batch) | 256 batches | merge into a per-app "rebind on drain" flag; the feature actor rebinds from the mirror snapshot |
| Reader → feature actor (find, map requests) | bounded mpsc | 16 | `Busy`; find answers lexical-only |
| feature actor → perceive-io | bounded mpsc | 1024 | cannot fill in steady state (bounded by the dedupe bounds above); overflow drops `Background` |
| perceive-io → feature actor | bounded mpsc | 1024 | bounded by credits + pending; never full |
| cpu jobs → Reader / Speech | as in ADR-0001 §6.2 | — | epoch-checked (E-3) |

The feature store is readable lock-free: a `ContentKey → Arc<Entry>` concurrent map (the same candidate as MS-3 / MB-1). Vectors are `Arc<[f32]>`. `FindJob`s read without messaging the actor.

### 5.2 Interfaces (sketch)

```rust
// gorca-perceive-api
pub enum Modality { Text, Visual, VisualFingerprint, Structure, Geometry }   // Interaction: not in this ADR
pub struct EncoderId { pub name: SmolStr, pub version: u32, pub dim: u16 }
pub enum PerceiveClass { Query, Name, Scope, Background }
pub trait PerceiveClient: Send + 'static {
    /// Never blocks. The reply arrives on the oneshot by `deadline + 5 ms` or the sender is dropped (W-1, C-2).
    fn submit(&self, req: PerceiveReq) -> Result<(), SubmitError>;        // Full | Unavailable
    fn cancel_before(&self, epoch: Epoch, class: PerceiveClass);
    fn health(&self) -> PerceiveHealth;
    fn encoders(&self) -> Arc<[EncoderInfo]>;
}
pub struct PerceiveReq { pub class: PerceiveClass, pub epoch: Epoch, pub deadline: Instant,
                         pub op: PerceiveOp, pub reply: oneshot::Sender<Result<PerceiveReply, PerceiveError>> }
pub enum PerceiveError { Timeout, Cancelled, Superseded, Overloaded, Unavailable, ModelUnavailable, BadInput }

/// Pixel acquisition (§10). Implementations live in gorca-capture-* and are wired by gorca-daemon.
pub trait PixelSource: Send + Sync + 'static {
    fn caps(&self) -> CaptureCaps;           // per-window / per-area, persistent?, indicator?, typical latency
    fn capture(&self, req: CaptureReq) -> BoxFuture<'static, Result<Frame, CaptureError>>;  // CaptureReq has a deadline
}

// gorca-features
pub enum Exclusion { PasswordRole, InsidePassword, EditableContent, SensitiveApp, SystemPrompt,
                     TerminalContent, SecretPattern /* X-11 */, UserOptOut, Paused, Offscreen /* visual only */, MaskTooLarge }
pub enum Eligibility { Allowed(NormalisedInput), Excluded(Exclusion) }
pub trait PrivacyPolicy: Send + Sync {
    /// Pure, O(depth) at worst (ancestor password check is cached per subtree revision).
    fn eligibility(&self, node: &NodeSnapshot, ancestry: &AncestryView<'_>, m: Modality) -> Eligibility;
}
pub enum FeatureState { Absent, Pending { deadline: Instant }, Ready(Arc<[f32]>), Failed { at: Instant, err: PerceiveError } }

// gorca-vector
pub trait VectorIndex: Send + Sync {
    fn dim(&self) -> u16;
    fn len(&self) -> usize;
    fn upsert(&mut self, id: ItemId, v: &[f32]);
    fn remove(&mut self, id: ItemId);
    /// Exact for ExactF32. Must honour `filter` before ranking (no post-filter recall loss).
    fn knn(&self, q: &[f32], k: usize, filter: &dyn Fn(ItemId) -> bool) -> SmallVec<[(ItemId, f32); 16]>;
}
```

The `MapSurface` and `MapLayout` sketches are in MM §8 and are adopted with the state machine in §8.1.

### 5.3 `VectorIndex` policy

- **VI-1:** `ExactF32` is the only implementation in Phases 1–4. Scope indexes (one page, document or window) have 10²–10⁴ items: microseconds to about 1 ms.
- **VI-2:** Index items are **unique content keys**, not nodes. A posting list `ContentKey → SmallVec<NodeKey>` expands hits, so 100 "Reply" buttons cost one vector and one distance.
- **VI-3:** `HnswF16` may be added (feature `ann`) only for an index that exceeds 1.5×10⁵ vectors (history, global label memory), and only after a measurement shows recall@10 ≥ 0.95 at the configured `ef`/`M`. **i8 quantisation is banned** (recall 0.45–0.70 in MM §7). Choosing between usearch (C++ FFI) and a pure-Rust HNSW is a microbenchmark task for perf-engineer, not a bake-off.
- **VI-4:** Scores are cosines on L2-normalised vectors. The SIMD dot product uses f32 accumulation; f16 storage is allowed for cold store entries if PM-3 shows no measurable ranking loss.

## 6. Privacy model

### 6.1 Principles

- **PR-0 Output is never filtered.** Speech, braille and navigation (focus, browse mode, review, find results rendered as speech) always present what the application exposes, including passwords shown in plain text, tokens, keys and terminal output: gorca reads everything. `PrivacyPolicy` and every `Exclusion` concern only what the perception layer **embeds** (vectors, content keys, OCR and fingerprint inputs) or **persists** (§6.4). No exclusion ever suppresses, masks or delays spoken or brailled output.
- **PR-1 Gate before key.** `PrivacyPolicy::eligibility` runs in core, before a `ContentKey` is computed. An excluded input has no key, so no code path can request, cache, persist or log it. Extensions (WASM) may *add* exclusions through a future hook. They can never relax one, and they never see vectors.
- **PR-2 Vectors equal text.** A vector has the privacy class of its input: same exclusions, same retention, same persistence rules.
- **PR-3 Local only.** No network in the sidecar (§4.6); no telemetry carrying content, vectors or full keys. Logs and traces may contain at most a 4-byte key prefix.
- **PR-4 RAM by default.** Only label memory (§6.4) persists in Phases 1–4.
- **PR-5 The sidecar is stateless** (W-2).

### 6.2 Exclusion rules (default policy)

| # | Rule | Modalities | Notes |
|---|---|---|---|
| X-1 | `Role::PasswordText` nodes (AT-SPI `password text`, ARIA/HTML `input[type=password]`) | all | The name of a password field ("Password") is excluded too. It is cheap to lose and avoids edge cases. |
| X-2 | Descendants of X-1 nodes | all | — |
| X-3 | Text content of nodes with `STATE_EDITABLE`, and every descendant of an editable root (rich editors, `contenteditable` documents) | Text | The accessible **name** or label of the editable field is allowed (MM §9.1). |
| X-4 | **Visual:** every crop has the bounds of all X-1/X-2 nodes and of focused editable nodes **blacked out before it leaves the capture code**. If the mask covers > 30 % of the crop, the crop is dropped (`MaskTooLarge`). | Visual, VisualFingerprint | Masking happens in `gorca-features`, before the memfd is sealed. |
| X-5 | **Sensitive apps** (user-editable denylist, matched on `AppInfo.app_name` / toolkit / executable): password managers (KeePassXC, Bitwarden, Seahorse), `gcr-prompter`, `pinentry-*`, `ssh-askpass`, polkit agents | all | This is configuration data matched generically, so the "core never branches on app name" rule (ADR-0001 §4.4) is kept. |
| X-6 | **System prompts:** GNOME Shell unlock, polkit and network-secret dialogs (shell modal dialogs with a password field) | all | Detected structurally: a modal dialog containing an X-1 node excludes the whole dialog subtree. |
| X-7 | **Terminal content** (VTE terminal text, ADR-0001 §5.6) is governed by the user setting `perception.terminal_content` = `off` (**default**) \| `on`, with an optional per-terminal-app override. When `off`, terminal content is excluded; terminal chrome (tabs, menus, profile dialogs) is always allowed. When `on`, terminal **text** is eligible line by line, after X-11. Terminal **visual** crops stay excluded until the pixel path (§10) can mask the row extents of X-11 matches (the mask comes from `Text.GetRangeExtents` on the matched rows); X-4's 30 % rule applies to that mask. Switching the toggle to `off` applies RT-4 at once. | Text (Visual: see note) | Owner amendment, 2026-10-12. Terminal output often holds secrets and churns fast, so it stays off unless the user asks for it. |
| X-8 | User opt-out per app, per site (web source layer), and a global **"pause perception"** command | all | Pausing also sends `CancelBefore(MAX)` (§4.5). |
| X-9 | Offscreen or not `SHOWING` | Visual only | Text of offscreen document content stays searchable; that is the point of find. |
| X-10 | Private or incognito browser windows, when the source layer can tell | all | Detection is open (O-5). |
| X-11 | **Obvious secrets, always filtered**, whatever X-7 or any other setting says. Applies to every text input that reaches `eligibility` (terminal lines, and also document and chrome text). Detectors, each pure and O(input length): **(a) password prompts:** a line matching the prompt set (`[Pp]assword`, `[Pp]assphrase`, `\bPIN\b`, `[Pp]asscode`, `Enter .* for .*:`, `[sudo] password for`, `Verification code`, OTP/2FA prompts, plus their Russian forms: `[Пп]ароль`, `[Кк]од подтверждения`), and **the next 3 lines** after it, because an echoing prompt can put the secret on the following line; **(b) private keys and certificates with keys:** everything from a `-----BEGIN … PRIVATE KEY-----` / `BEGIN OPENSSH PRIVATE KEY` / `BEGIN PGP PRIVATE KEY BLOCK` line through the matching `END` line, or through 200 lines if no `END` arrives; **(c) tokens with a known format:** prefixes such as `ghp_`, `gho_`, `github_pat_`, `glpat-`, `xox[abposr]-`, `sk-`, `sk_live_`, `rk_live_`, `AKIA`/`ASIA` + 16 chars, `AIza`, `ya29.`, `npm_`, `pypi-`, and JWTs (`eyJ…\.eyJ…\.…`); **(d) assignments:** `(password\|passwd\|secret\|token\|api[_-]?key\|auth\|bearer)\s*[:=]\s*\S+` and `Authorization:` headers; **(e) high-entropy runs:** a whitespace-free run of ≥ 32 characters in the base64, base64url or hex alphabet with Shannon entropy ≥ 4.0 bits per char (base64) or ≥ 3.0 (hex). A match **drops the whole line** (and the ranges in (a) and (b)), never a span, so no partial secret leaks into a key. The pattern list is configuration data and can be extended by the user and by extensions (PR-1), never shrunk below this built-in set. | Text (and Visual via the row mask, see X-7) | Owner amendment, 2026-10-12. False positives (a hash, a UUID-like id) cost one unsearchable line; false negatives are the risk, so detectors favour recall. |

AT-SPI `STATE_SENSITIVE` means "enabled for input", not "private". It plays no part in this policy.

### 6.3 Retention (RAM)

- **RT-1 Binding-scoped.** An entry is referenced while at least one live mirror node binds to it (§7.2). With refcount 0, it is evictable on an LRU basis.
- **RT-2 Budget.** The feature store defaults to 96 MiB (about 64k × 384 f32 vectors) and counts against ADR-0001 M-8. Referenced entries are never evicted. If live bindings exceed the budget, only the focused app's active scopes get new encodings.
- **RT-3 Session end.** The store is dropped; buffers are zeroed on drop (best effort).
- **RT-4 Pause and opt-out** drop matching entries immediately, referenced or not.

### 6.4 Persistence (label memory only, Phase 2; proposed)

| Item | Decision |
|---|---|
| What | Label memory records: `{ fingerprint: u64 phash + 1024-f32 ink mask (Phase 3), structural context hash, app id string, role, label text, source: User / SeenLabelled / Ocr, confidence, last_seen }`. This is control chrome only, never document content, editable content or anything excluded by X-1…X-10. |
| Where | `$XDG_STATE_HOME/gorca/labels.db` (default `~/.local/state/gorca/`); directory 0700, file 0600 |
| Format | Append-only record log plus periodic compaction. Each record is sealed with XChaCha20-Poly1305. |
| Key | A 256-bit key from the **Secret Service** (`org.freedesktop.secrets`, item `gorca label memory`), created on first use. The key provider is in `gorca-dbus` and its fetch is `Startup`-class. |
| Keyring locked or absent (greeter, other desktops) | Label memory runs **in RAM only** for that session. **There is no plaintext fallback** unless the user explicitly sets `labels.persist = "plaintext"`. |
| Retention | Entries unseen for 180 days are compacted away. A "forget labels for this app" command and a "forget all" command exist. |
| History search | **Out of scope for this ADR.** It is a reading log ("essentially a keylogger of reading", MM §7) and needs its own decision: off by default, per-app opt-in, TTL. |

## 7. Vector key scheme and lifecycle

### 7.1 `ContentKey`

```
ContentKey = BLAKE3_keyed(K_user, "gorca.ck.v1" ‖ modality_tag ‖ view_version ‖ normalised_input)[0..16]
```

- **Keyed hash (proposed).** `K_user` is a per-user 256-bit secret, stored next to the label-memory key, or random per session when there is no keyring. An unkeyed hash of a short string ("Yes", a username) can be reversed by dictionary from a log or dump; a keyed one cannot.
- **The encoder is not in the key.** The store maps `(ContentKey, EncoderId) → FeatureState`. An encoder upgrade (new `EncoderId.version` or `model_sha256` in `HelloAck`) makes old vectors unreachable without touching keys, and they are GC'd by the LRU.
- **Text view `text/v1`:** `name ⏎ description`, plus, for non-editable text leaves (paragraph, heading, static text, list item, cell), their text content. The input is NFC-normalised, whitespace is collapsed and trimmed, case is preserved, and the input is truncated at the encoder's token limit (512 for e5) *before* hashing, so the key hashes exactly what is encoded. The role is not in the text view; role filters are structural (VI-2 posting lists carry the role).
- **Visual view `visual/v1`:** the masked crop, resized to the encoder's input size (224×224, bicubic, MM §10), hashed over the resized RGB buffer. `VisualFingerprint` uses the ink mask, which is the cross-theme identity (ink-mask cosine 0.90 top-1, MM §5).
- **Collisions:** 128 bits truncated from BLAKE3. The probability over 10⁷ keys is about 1.5×10⁻²⁵, so collisions are ignored.

### 7.2 Binding to mirror nodes

The app or doc actor owns a per-app `NodeBindings` table (single writer): `NodeKey → { text: Option<ContentKey>, visual: Option<ContentKey>, view_rev }`.

| Mirror event (ADR-0001 §4.4) | Action in the app/doc actor | Feature actor |
|---|---|---|
| Node appears (snapshot applied) | Evaluate eligibility. If `Allowed`, compute the key (BLAKE3 of about 100 B, < 1 µs). Emit `Acquire(key)`. | refcount += 1. If `Absent` and the node is in an **active scope**: request `Scope`-class encoding |
| `PropertiesChanged` (name, description, role, states) | Recompute eligibility and key. If either changed: `Release(old)`, `Acquire(new)`, or `Release(old)` only if now excluded. | as above. If the node became excluded, its old entry is released, never re-requested |
| `TextChanged` on a text leaf | Mark the binding dirty. Rekey lazily when the scope is next encoded or searched (text churns; do not encode per keystroke). | — |
| `NodeRemoved`, `Defunct`, app `Gone`, doc actor rebuild | `Release(key)` for each binding | refcount −= 1; at 0 → LRU (RT-1). A re-render that reproduces the same content hits the cache. |
| Scope becomes active (window focus, doc load-complete, find invoked) | — | Enqueue `Absent` keys of the scope in document order, with the viewport first (ADR-0001 §9) |

- **K-I1:** a key is computed only from input that was eligible at that moment. The normalised input is a snapshot, so the sidecar never sees content that was ineligible at request time.
- **K-I2:** a reply for a key whose refcount is 0 and that was released *because of exclusion* is discarded, not stored. The feature actor keeps a small "tombstoned-by-policy" set for in-flight keys.
- **K-I3 Lazy, scoped encoding.** Only active scopes are encoded: the focused window's tree, the current document, and the scope of a running find. Background apps are not encoded, which saves CPU and limits the exposure of what is held in RAM.
- **K-I4:** find results and map items are validated against the mirror at presentation (`Lookup::Gone` → skipped). Indexes are tagged with the mirror revision they were built from.

## 8. MapSurface

### 8.1 State machines

```rust
enum MapState {                                   // per scope, owned by the feature actor, published as ArcSwap<MapSnapshot>
    Empty,
    Building { gen: u64, started: Instant },      // no snapshot yet
    Ready    { gen: u64 },
    Updating { serving: u64, next: u64 },         // navigation reads `serving`
    Failed   { serving: Option<u64>, err: MapError },
}
enum MapNav {                                     // Reader-owned, inside Mode::Map
    Overview { scope: MapScope, gen: u64, cell: CellId, lens: Lens, path: SmallVec<[CellId; 3]> },
    Members  { scope: MapScope, gen: u64, cell: CellId, idx: u32, path: SmallVec<[CellId; 3]> },
}
enum Lens { Topic, Kind, Mixed }                  // fused weights (1,0.25,0.1) / structure-heavy / (1,0.5,0.25), MM §3
```

| From | Event | To | Notes |
|---|---|---|---|
| Empty | open map on scope | Building | `MapBuild` on `cpu`; earcon `building` if still building after 300 ms |
| Building | build done | Ready | Reader enters `Overview` at the cell of the focused item |
| Ready | item delta (bindings changed) | Updating | debounced 500 ms; anchored refit + hysteresis (MM §6.4) |
| Updating | update done | Ready{next} | the Reader's `gen` moves; the cursor keeps its `CellId`; see §8.3 |
| Building, Updating | error or deadline (build 2 s, update 200 ms) | Failed | navigation keeps `serving` if present |
| any | scope closed or app `Gone` | Empty | the Reader leaves `Mode::Map` with an announcement |

Step tables (`step(cell, dir)`, empty cells skipped) and cell labels (top 2–4 TF-IDF terms, count, dominant role) are precomputed per generation, so every Reader operation is O(1) (ADR-0001 M-14).

### 8.2 Commands (`Mode::Map`; key bindings are provisional, `docs/design/modes.md`)

| Command | Default key | Effect |
|---|---|---|
| Open map | gorca+M | Opens a map of the current scope (document, else focused window); enters `Overview` at the focused item's cell |
| Move | Arrows | `step` to the nearest non-empty cell in that direction |
| Zoom in | Enter | Opens a sub-map if the cell has > 30 members, else `Members` |
| Members next / previous | Down/Up, or Tab/Shift+Tab, in `Members` | Moves through the cell's members in document order |
| Activate | Space | Moves focus (or the browse cursor) to the current member, or to the first member of the cell; exits the map |
| Zoom out | Escape, Backspace | Pops one level; at the top, exits the map and restores the previous mode and position |
| Cycle lens | L | Topic → Kind → Mixed; rebuilds the map; the cursor follows the cell of the current anchor item |
| Where am I | W | Zoom path, row and column, label, count |
| Focus on map | F | Jumps to the cell of the item that had focus before the map opened |

### 8.3 Speech and earcon contract

- **SE-1 Order:** the earcon starts first (≤ 40 ms tone), and speech is submitted in the same Reader turn. Every move is an `Interrupt` with a new epoch, so the previous cell's speech is cancelled (ADR-0001 E-2). Holding an arrow key produces earcons, and only the last cell is spoken.
- **SE-2 Cell utterance:** "‹terms›, ‹count› items, mostly ‹role›", for example "fonts, size, typeface, 23 items, mostly buttons". Coordinates are spoken only by Where am I, or when the verbosity setting asks for them.
- **SE-3 Earcons:**

| Earcon | When | Parameters |
|---|---|---|
| `map.move` | each successful step | stereo azimuth = column, pitch = row |
| `map.skip` | a step skipped empty cells | a short glissando; length scales with the number skipped |
| `map.edge` | no non-empty cell in that direction | a dull thud; nothing is spoken |
| `map.zoom_in` / `map.zoom_out` | level change | rising / falling |
| `map.building` | build longer than 300 ms | repeats every 1 s |
| `map.updated` | the current cell's membership changed by > 20 % under the user | soft; no speech |
| `map.failed` | `Failed` with no `serving` generation | followed by "map unavailable" |

- **SE-4 Stability promise:** while the user is in a map, an item that did not change cannot move cells faster than the bound of Proposition 2 (MM §6.4; ε = 0.25 × median QE²). The cursor is anchored to a `CellId`, never to an item, in `Overview`; in `Members` it is anchored to an item.
- **SE-5 Earcon output path (TBD-E1, proposed amendment to ADR-0001 §8.2).** ADR-0001 has no earcon command. This ADR proposes `SpeechCmd::Earcon { id: EarconId, pan: i8, pitch: i8 }` with a capability `SpeechCaps.earcons: EarconCaps { pan: bool, pitch: bool }`. Lowering: voicego native is TBD (go-engineer); SSIP uses `SOUND_ICON name` (no pan or pitch; `refs/speech-dispatcher/doc/speech-dispatcher.texi:2835`). When pan and pitch are not available, `map.move` falls back to speaking the coordinates.

## 9. Features

### 9.1 Semantic find (Phase 1)

- **Flow:** the find command enters `Mode::Find` (an addition to ADR-0001 §7.2). Keys are grabbed into a gorca-owned query buffer; Russian input relies on the keysym/unicode field of `KeyEvent` (open question O-6). Enter submits; arrows walk the results; Enter activates; Escape restores the previous mode.
- **Ranking (proposed): hybrid.** Lexical (exact or prefix match on name, then token match on name, description and text) and semantic (e5 cosine) results are fused by reciprocal rank fusion. An exact name match is always ranked first, so "send" finds the Send button.
- **Anytime results:** lexical results are available at once from the mirror. The query vector (`Query` class, 50 ms) and the scope vectors are combined as they arrive. If semantic results change the top result within 150 ms, the first result is re-spoken (marked `may_refine`, ADR-0001 §8.1). After that, results are stable until the user acts.
- **Without the sidecar**, the feature is lexical-only (§4.5); find always works.
- **Speech:** "‹n› results" then the first result as a normal focus presentation. Scores are never spoken.

### 9.2 Unlabeled-control naming cascade (Phases 2–3)

Trigger: a focusable control with an empty name (after the toolkit name-from-content rules) receives focus or is visited in browse mode.

| Step | Source | Spoken form | Needs |
|---|---|---|---|
| 0 | immediate | "button, unlabeled" (role + state, as today) | nothing |
| 1 | non-visual hints: tooltip, description, `labelled-by`, web source-layer attributes (icon font class, `src` file name) | the label, plain | mirror |
| 2 | label memory: structural context hash + app (Phase 2), visual fingerprint (Phase 3) | a user-given label is spoken plainly; a seen-labelled-elsewhere match is spoken as "probably X" | feature store |
| 3 | OCR on the masked crop (text-bearing controls) | the text, plainly, if conf ≥ threshold | pixels + sidecar |
| 4 | SigLIP closed vocabulary (ru/en labels, MM §8) | "maybe X", only if margin ≥ τ, where τ is calibrated so that precision ≥ 0.9 (MM §5: 0.95 at 25 % coverage) | pixels + sidecar |
| — | abstain | nothing further | — |

- **NC-1:** steps 1–4 never delay step 0. An amendment is spoken only if the epoch is unchanged (focus still on that control) and it arrives within the `Name` deadline (500 ms). Otherwise it is cached for the next visit.
- **NC-2:** a "label this control" command stores a user label (source `User`), which overrides every automatic step.
- **NC-3:** cascade results are cached per `NodeKey` for the node's lifetime and per fingerprint in label memory. Captures are never repeated for a control already named.

## 10. Screen-pixel acquisition (open; owner: linux-platform-engineer)

Phases 1–2 need no pixels. Phase 3 is blocked on this decision. Facts checked in source:

- `org.gnome.Shell.Screenshot` accepts only `org.gnome.SettingsDaemon.MediaKeys` and `org.freedesktop.impl.portal.desktop.gnome` through `DBusSenderChecker` (`refs/gnome-shell/js/ui/screenshot.js:2848-2851`), unless the shell runs in unsafe mode (`refs/gnome-shell/js/misc/util.js:400`).
- `org.gnome.Mutter.ScreenCast` checks only that the caller is the session creator (`refs/mutter/src/backends/meta-screen-cast-session.c:268-273`). `RecordWindow` takes `window-id`; `RecordArea` takes stage coordinates, and the stream size depends on monitor scale (`refs/mutter/data/dbus-interfaces/org.gnome.Mutter.ScreenCast.xml:99-157`).
- **Indicators:** handles with `is-recording=true` show the privacy "recording" indicator. Handles without it show the screen-sharing indicator, which stays visible for at least 5 s (`MIN_SHARED_INDICATOR_VISIBLE_TIME_US`, `refs/gnome-shell/js/ui/status/remoteAccess.js:12, 36-60, 170-200`). Every on-demand grab therefore flashes an indicator for at least 5 s.
- **Wayland coordinates:** clients do not know their global position, so AT-SPI screen extents are unreliable. Window-relative extents plus a per-window capture avoid the problem; area capture needs a mapping from window to stage.

| Option | Indicator | Prompt | Latency | Coordinates | Notes |
|---|---|---|---|---|---|
| A. Mutter ScreenCast `RecordWindow`, on demand (open → 1 frame → close) | sharing indicator, ≥ 5 s per grab | none | unmeasured; target ≤ 150 ms | window-relative ✔ | needs AT-SPI window → Mutter `window-id` mapping (unknown) |
| B. Mutter `RecordArea`, on demand | same as A | none | unmeasured | stage coords; needs window position | overlapping windows and popups appear in the crop |
| C. Persistent ScreenCast stream while a perception feature is active | permanent | none | one frame (≤ 16 ms) after setup | either | GPU/CPU cost of a live stream; an honest but permanent indicator |
| D. Portal `ScreenCast`, `persist_mode=2` | permanent while open | one dialog, restore token | as C | per window/monitor | portable to non-GNOME desktops |
| E. Portal `Screenshot`, non-interactive | none | one dialog; grant covers all unsandboxed apps | slow (full-screen PNG) | full screen | writes PNG to disk, which violates "frames stay in RAM"; fallback only |
| F. Browser source layer (CDP `Page.captureScreenshot` with clip, or a WebExtension) | none | extension install | fast | DOM coordinates ✔ | web content only (`docs/reports/2026-10-12-web-source-layer.md`) |
| G. Upstream: an a11y capture method in Mutter (alongside `org.freedesktop.a11y.*`), or adding the `org.gnome.Orca.KeyboardMonitor` owner to the Screenshot allowlist | none, or a11y-specific | none | fast | either | long lead time; needs a GNOME design discussion |

**Measurements requested from linux-platform-engineer:** for A, B and C, session setup and first-frame latency (p50/p99); indicator behaviour and duration; window-id mapping from an AT-SPI frame; HiDPI scale handling; whether `cursor-mode=hidden` and the exclusion of other windows hold; CPU cost of an idle persistent stream (C). Decision criteria: per-grab p95 ≤ 150 ms, no on-disk pixels, the user can always tell when capture is happening, and a correct crop for a node with window-relative extents. Recommended interim: **F for web, A for native apps**, pending numbers.

## 11. Performance budgets

| Item | Budget | Source |
|---|---|---|
| Effect on ADR-0001 M-1 (key → speech) while the sidecar encodes at its maximum bulk rate | Δp99 < 0.5 ms | PM-1 |
| Key computation per node (eligibility + BLAKE3) | < 1 µs p99; in the app actor's batch, never in the drain | — |
| Query encode (e5 int8, sidecar compute) | 1.8 ms p50 / 3.4 ms p99 (MM §4); with IPC ≤ 5 ms p99 | PM-2 |
| `knn` exact, 10⁴ × 384 | ≤ 1 ms p99, one core | PM-8 |
| `knn` exact, 1.5×10⁵ × 384 | ≤ 10 ms p99, one core (on `cpu`, never the Reader) | PM-8 |
| Find: Enter → first result `Speak` written, warm scope | ≤ 15 ms p99 lexical; ≤ 30 ms p99 including semantic | PM-3 |
| Find: cold scope of 2k nodes, semantic results complete | ≤ 400 ms p95 (2k × 1.8 ms / 4 threads + batching) | PM-3 |
| Bulk encoding | ≤ 1 core average, idle-priority; ≤ 2000 inputs/s | PM-1 |
| Map build, 2k items × 64 cells × 384–411 dims | ≤ 100 ms p95 (30 iterations × about 2 ms per BMU pass, MM §10) | PM-10 |
| Map update (anchored + hysteresis) | ≤ 10 ms p99 | PM-10 |
| Map navigation step on the Reader | ≤ 50 µs p99 (table lookup) | PM-10, ADR-0001 M-14 |
| Naming cascade: focus → amendment spoken (Phase 3) | p95 ≤ 300 ms; never delays step 0 | PM-12 |
| SigLIP crop / OCR line | 39 ms (4 threads) / 6–18 ms (MM §4) | — |
| Sidecar cold start to `Ready` with e5 loaded | ≤ 1.5 s (unmeasured) | PM-9 |
| Sidecar restart after a crash (socket activation) | ≤ 2 s to `Ready` | PM-6 |
| Memory: feature store | ≤ 96 MiB default, inside M-8's 600 MB | PM-7 |
| Memory: sidecar | ≤ 400 MB RSS with e5 + SigLIP + Tesseract eng+rus; `MemoryMax=768M` | PM-7 |

## 12. Metrics to validate

| ID | Metric | Target | How measured |
|---|---|---|---|
| PM-1 | hot-path non-interference | ADR-0001 M-1 Δp99 < 0.5 ms and M-3 unchanged while bulk encoding runs flat out; Reader occupancy (M-14) unchanged | perf harness + real sidecar |
| PM-2 | sidecar latency | Query p99 ≤ 5 ms end to end over the socket; interactive head-of-line wait behind bulk ≤ 10 ms p99 | proto bench |
| PM-3 | find latency and quality | latencies in §11. On a labelled ru+en query set (GNOME Settings, help corpus, `tests/web/` corpus): exact-name queries top-1 ≥ 0.98; MRR@10 hybrid ≥ lexical-only + 0.15; cross-lingual (en query → ru UI) top-5 ≥ 0.6 | eval harness (python-engineer builds the query set) |
| PM-4 | **privacy conformance** | 0 frames carrying a canary placed in password fields, editable content, denylisted apps or system prompts, across a fuzzed tree corpus and 10k random mutations; the sidecar self-test denies `socket()`; 0 files written by the sidecar (fanotify or audit) | testkit canary harness + CI |
| PM-5 | deadline conformance | 100 % of requests resolved by `deadline + 5 ms` with the sidecar `SIGSTOP`ped, slowed by 10×, or killed mid-batch | testkit |
| PM-6 | crash isolation | `kill -9` of the sidecar during an M-3 storm: no M-1 change; back to `Ready` ≤ 2 s; 3 crashes in 60 s → `Quarantined`, announced at most once | testkit |
| PM-7 | memory | store ≤ budget; sidecar RSS ≤ 400 MB; no growth over the 8 h soak (M-8) | smem |
| PM-8 | exact index | `knn` budgets in §11; the Rust scan matches the faiss flat baseline within 10 % | `gorca-bench` |
| PM-9 | model load | cold start to `Ready` per encoder | sidecar bench |
| PM-10 | map | build/update/step budgets; `research/multimodal/testvec-som.json` codebook within 1e-6 and assignments exact; untouched-item moves ≤ 0.1 % per update (MM §6.3 measured 0.008 %) | `gorca-bench` + unit tests |
| PM-11 | **map usefulness (user study)** | with blind participants, task success and time against linear navigation and against find; go/no-go for default-on | user study protocol (separate doc) |
| PM-12 | naming cascade | spoken "maybe X" precision ≥ 0.9 at reported coverage on Adwaita + a web icon set; amendment p95 ≤ 300 ms | labelled crop set (chrome only) |
| PM-13 | cache efficiency | content-key hit rate ≥ 90 % on the SPA re-render corpus (`tests/web/`) | testkit replay |
| PM-14 | idle cost | sidecar 0 % CPU and 0 wakeups/s when idle; exits after `--idle-exit` | `perf sched`, powertop |
| PM-15 | **secret filter (X-11)** | recall 100 % on a fixed corpus of terminal transcripts with planted secrets (sudo/ssh/gpg prompts, PEM and OpenSSH keys, the token formats in X-11, JWTs, `.env` assignments); false-positive rate reported on 10k lines of ordinary build and shell output; detector cost ≤ 2 µs per 100-byte line; with `terminal_content = off`, 0 terminal keys created | unit corpus in `gorca-features` tests + testkit replay |

## 13. Phased rollout

| Phase | Scope | Exit criteria |
|---|---|---|
| **0. Scaffolding** | new crates (§3.1), xtask rules 8–12, proto + codec with fuzz target, sidecar skeleton with sandbox and self-test plus a dummy encoder, `perceive-io` + health SM, `FakePerceive` in testkit, systemd units | `make check` green with the new rules; PM-4 (sandbox part), PM-5, PM-6 with the dummy encoder |
| **1. Semantic find** | e5-small int8 in the sidecar; `gorca-models fetch`; feature store (RAM), privacy policy X-1…X-10, bindings; `ExactF32`; hybrid find; `Mode::Find`; lexical fallback | PM-1, PM-2, PM-3, PM-4 (full), PM-7, PM-8, PM-13. Find ships **on by default**; without models, it is lexical-only and says so once |
| **2. Label memory (no pixels)** | encrypted persistence (§6.4); steps 0–2 of the cascade with the structural context hash; "label this control"; forget commands | PM-4 extended to persistence; recovery from a corrupt `labels.db` (record skipped, never a crash) |
| **3. Pixels** | §10 decision and the `gorca-capture-*` backend; masking (X-4); `Fingerprint`, Tesseract (eng+rus), SigLIP + vocab; cascade steps 3–4 | PM-12; capture budget from §10; no on-disk pixels (audit) |
| **4. MapSurface (flag)** | `gorca-map` + `SomLayout` (Rust port, testvec), `Mode::Map`, earcons (TBD-E1), lenses | PM-10 in CI; PM-11 study. Default-on only after PM-11 passes |
| **5. Later (separate decisions)** | `HnswF16` (VI-3), history search (opt-in, TTL), on-demand image captioning (VLM), GPU execution provider | each needs its own ADR or decision entry |

Phase 4 does not depend on Phases 2–3 and can run in parallel with them once Phase 1 has landed.

## 14. Alternatives considered

| Alternative | Why rejected |
|---|---|
| Encoders as WASM components | Model size, SIMD and threads; ORT and candle do not run well in wasmtime (MM §0a). The extension sandbox is for policy code, not for 100 MB models. |
| ORT in the gorca process | A crash or hang in C++ inference would take down the reader; there would be no network sandbox separate from gorca, which needs D-Bus; and memory would be charged to the core. |
| D-Bus between gorca and the sidecar | It requires session-bus access from the sidecar, which is an exfiltration path (§4.1), and it brings broker semantics onto a private link. |
| Local TCP, gRPC or HTTP | Requires `AF_INET` in the sidecar, which defeats the network ban. |
| Shared-memory ring for every message | Unnecessary: control messages are small. memfd is used only for images. |
| Store keyed by node identity | Loses re-render and duplicate sharing (MM §0b); keys churn with the GTK text-run ids and web re-renders. |
| ANN from day one | Not needed below 1.5×10⁵ vectors; adds approximation and a C++ dependency (MM §7). |
| t-SNE / UMAP / GTM / Growing Grid layouts | Unstable on retrain, empty grids or unpredictable shapes (MM §6.3, §6.6). |
| Pixels captured by the sidecar | The sidecar would need D-Bus and PipeWire, which conflicts with "no `socket()`" (§4.6). |
| Plaintext label memory by default | User-given labels can be personal; a keyring exists on the target desktop. Plaintext is opt-in only. |
| Lexical-free, pure semantic find | Exact-name queries degrade, and find would fail whenever the sidecar is down. |

No bake-off is proposed. The contested points are settled by measurement: pixel capture (linux-platform-engineer, §10), the ANN implementation (perf-engineer microbenchmark, VI-3), and ORT thread tuning (PM-2).

## 15. Consequences

**Positive**

- Perception can crash, hang or be absent without any effect on speech, focus or navigation (PM-1, PM-5, PM-6).
- "No network" holds in three places that can each be checked: the dependency graph (rule 10), the process (seccomp, Landlock and the self-test), and the unit.
- Content keys turn re-renders and duplicate controls into cache hits; the encoding cost follows unique content, not tree size.
- Find works on day one and on every machine, with or without models.

**Negative / costs**

- A second binary and a C++ runtime (libonnxruntime) to package, plus 120 MB to 400 MB of models that users fetch explicitly. Debian packaging of ORT is unverified (O-4).
- The seccomp and Landlock profiles must be maintained as ORT and Tesseract change (a new syscall turns into `EPERM` and a feature failure, which is caught by the sidecar self-test and PM-4).
- Encrypted label memory depends on the Secret Service; at the greeter and on keyring-less desktops it is session-only.
- The privacy policy is conservative (X-1, X-7, X-11): password-field names are never searchable, terminal content is unsearchable unless the user turns it on, and lines that look like secrets are dropped even then. Detector false positives make some harmless lines (hashes, long ids) unsearchable.
- `Mode::Find`, `Mode::Map` and `SpeechCmd::Earcon` extend ADR-0001 interfaces and need the owners of `docs/design/modes.md` and of the speech service to follow.

## 16. Open questions

| ID | Owner | Question |
|---|---|---|
| O-1 | linux-platform-engineer | Pixel acquisition (§10): measurements and recommendation. |
| O-2 | linux-platform-engineer | Mapping an AT-SPI frame (window) to a Mutter `window-id` for `RecordWindow`. |
| O-3 | linux-platform-engineer | Which of `PrivateNetwork=`, `IPAddressDeny=`, `MemoryDenyWriteExecute=`, `MemoryMax=` take effect in a Debian 13 systemd **user** unit (user namespaces, BPF, cgroup delegation). |
| O-4 | linux-platform-engineer | ONNX Runtime on Debian 13: a distro package, or a pinned vendored `libonnxruntime.so`? Tesseract `rus` traineddata packaging. |
| O-5 | web-a11y-engineer | Can the source layer tell private or incognito windows (X-10)? |
| O-6 | linux-platform-engineer | Text entry in `Mode::Find`: does `KeyEvent` carry enough (keysym/unicode, layout) for Russian and dead keys, or is an IM path needed? |
| O-7 | go-engineer | voicego earcon support with pan and pitch (TBD-E1). |
| O-8 | math-researcher | Calibration of the SigLIP margin τ and the OCR confidence threshold on a web icon set; verification of the WebAIM and embedding-inversion citations (MM §11). |
| O-9 | wasm-engineer | WIT hook for extensions to *add* exclusions (PR-1), without vector access. |

## 17. Sign-off (all items approved 2026-10-12; item 6 amended)

1. **Sandbox (§4.6):** in-binary seccomp and Landlock are mandatory and fail-closed (perception never runs unsandboxed), with the systemd unit as a second layer.
2. **Activation (§4.1):** a socket-activated systemd user unit with idle exit, rather than a child process spawned by gorca; one active client per sidecar.
3. **Pixel capture in the gorca daemon, not the sidecar** (§2 item 4, §10).
4. **Label memory persistence (§6.4):** encrypted with a Secret Service key; RAM-only when there is no keyring; plaintext only by explicit opt-in.
5. **Keyed content hash (§7.1)** with a per-user secret.
6. **Default exclusions (§6.2):** X-1 also excludes the *name* of password fields; X-7 excludes terminal content by default. *Approved with amendment:* X-7 is a user toggle (default off), and X-11 always filters obvious secrets.
7. **Hybrid find with a lexical fallback, on by default (§9.1, §13 Phase 1).**
8. **`SpeechCmd::Earcon` (TBD-E1)**, an amendment to ADR-0001 §8.2.
9. **`Mode::Find` and `Mode::Map`**, an amendment to ADR-0001 §7.2; the map stays behind a flag until PM-11 passes.
10. **History search stays out of scope** until a separate decision.
