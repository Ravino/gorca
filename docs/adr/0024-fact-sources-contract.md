# ADR-0024: Fact sources: taxonomy, contract, identity, trust and confidence for the semantic model

Status: **Proposed** (2026-10-19). Owner: architect. Amends ADR-0022 r2 §2.2 (layers and source
slots); ADR-0022 r3 points here.

- Kickoff analysis, prior art, worked examples: `docs/reports/m22a-kickoff.md`.
- Implementation plan for the parts M22a builds: `docs/plan/m22a-spec.md`.
- Builds on: ADR-0022 r2 (registers, `FUSE`, `SemEntry`, `ProvTag`, `explain`, SM-1..SM-8),
  `docs/reports/semantic-fusion-math.md` (Lemma 1–3, Theorem 1, F1–F10), ADR-0001 §4.1 (identity
  I-1..I-3), §6.2 (channels), §9.3 (IR-1..IR-8, TBD-S1..S4), ADR-0002 (perception sidecar, privacy
  PR-0..PR-5, exclusions X-1..X-11, naming cascade NC-1..NC-3), ADR-0003 (doc mirror), ADR-0004
  (ingress coalescer), ADR-0019 (worlds, effects buffer, I-1..I-8, V-1..V-8, permissions).
- Milestones: M22a (contract, AT-SPI source, fake source), M19 (AccessKit/Newton), M18a (WIT 0.9
  `facts`), **M23** (browser add-on bus), **M24** (local inference facts), **M25** (annotations:
  summaries, long descriptions). `docs/plan/phase-3.md` §M22, §M23–M25.

## 1. Context

The owner decided on 2026-10-19 that Phase 3 starts with M22a and that the semantic model's source
contract must be open to future fact sources, all of them provenance-tagged and confidence-weighted
contributors to one model:

| Source | What it knows | Trust | Arrives |
|---|---|---|---|
| AT-SPI2 (today) | the toolkit's or engine's own accessibility tree | platform | events + deadline-bounded fetches |
| Newton / AccessKit (M19) | the same, Wayland-native, whole-tree updates | platform | tree updates per surface |
| Browser add-on bus (M23): a Firefox/Chrome WebExtension over native messaging or a gorca socket | DOM, ARIA source attributes, CSS visibility, layout boxes, icon classes, `src` names, shadow DOM, framework hints | page-influenced, untrusted | pushed mutation batches |
| gorca WASM extensions (M18a) | app- or site-specific knowledge | add-on: first-party or third-party | effects after a hook returns |
| Local ML inference (M24/M25): OCR of in-memory captures from the gorca-helper shell extension, image/icon/UI-element description, summarisation | pixels and text interpreted by a model | model output, uncalibrated until calibrated | seconds later, on demand or in bulk |

ADR-0022 r2 already has the right core: one register per (node, source slot, group), a pure `FUSE`
evaluated by the node's single writer (SM-8), a static per-group layer order and trust table `PI`,
`Unknown` vs `Absent`, provenance beside the value, `explain`. Its gaps for this list:

1. The slot list is closed (`atspi`, `accesskit`, `user`, `patch`).
2. Every subject is a `NodeKey` the primary connector minted. A browser add-on names DOM elements,
   OCR names a pixel region, neither knows an AT-SPI path.
3. There is no notion of confidence, of evidence that must be interpreted before it means
   anything (an icon class is not a name), or of a fact going stale because the screen changed.
4. Every source is trusted as much as AT-SPI. Nothing bounds a flooding or hostile source, and
   nothing says which groups a source may touch.
5. Nothing says what may happen on the key → speech path when a fact arrives late.

Prior art (details and citations in the kickoff report §2): Chromium splices image annotations
into the accessible name, status strings included (`refs/chromium/ui/accessibility/platform/ax_platform_node_base.cc:235-262`),
so the reader cannot tell model output from author text. NVDA keeps OCR out of the model: a
user-invoked recogniser produces a separate virtual document (`refs/nvda/source/contentRecog/recogUi.py:95,291`).
ADR-0002 §9.2 already defines a gorca cascade with hedged wording ("probably X", "maybe X") and
calibrated abstention. The math report rejected numeric confidence for *ranking* across sources
(uncalibrated, permutation-free only with a static order, unexplainable; §1.3, alternative G of
ADR-0022).

## 2. Decision

### 2.1 Source taxonomy

Every contributor is a **fact source** with a `SourceDescriptor` the core validates once. A source
belongs to exactly one class; the class fixes the highest layer it can write, what it may create,
and which transport it uses.

| Class | Examples | Highest layer | May own existence | Subjects | Transport | Default trust tier |
|---|---|---|---|---|---|---|
| **Primary** | AT-SPI connector + doc source, AccessKit/Newton connector | Observed (S0) | yes, real nodes | `NodeKey` it mints | in-band with events (ADR-0004 ingress, doc source deltas) | T1 platform |
| **Secondary** | browser add-on bus; a privileged Firefox source (ADR-0001 TBD-S2) used as a fact contributor | Observed (S0), ranked by `PI` after the primary by default | no | anchors (§2.3) | `FactPort` (§2.6) | T3 untrusted |
| **Patch** | add-on patches (ADR-0019 PA-1, selectors) | Patch (S1) | no (only `hide`) | selector matches | ext host, epoch | T2/T3 by manifest |
| **User** | overrides, label-this-control (NC-2) | User (S2) | no | `NodeKey` / label memory key | settings | T0 |
| **Inferred** | OCR, icon/image/UI-element models, label memory "seen elsewhere", extension facts (§2.10) | Inferred (S4, below Derived) | **synthetic nodes only** (§2.3.4), later | anchors incl. regions | `FactPort` | T2 (gorca sidecar) / T3 (extension) |
| **Annotation** | summaries, long image descriptions | none: never fused into `Semantics` | no | subtree or region | `FactPort`, on demand | as producer |

Annotations are **content**, not semantics: they are spoken only by an explicit command (or a
user-enabled automatic rule) as declared content utterances (ADR-0022 §2.9), carry their source and
are never part of a `SemEntry`, a name or a cache key.

A fact also has a **kind**:

- **Semantic fact**: a value for an ADR-0022 group (`role`, `name`, `check`, `setpos`, ...).
- **Evidence fact**: an input that rules must interpret before it means anything (`hint.icon_class`,
  `hint.src_name`, `hint.title_attr`, `visual.ocr_text`, `visual.label_guess`, `layout.box`,
  `layout.css_hidden`). Evidence lives in a sparse side table, is never presented directly, and is
  read only by the Derived stratum S3 (deterministic rules, e.g. "a `title` attribute names an
  unnamed control") and the Inferred stratum S4 (the naming cascade).

### 2.2 Layers and strata (amends ADR-0022 §2.2)

Precedence per group, highest first:

**user > patch > observed (primary, then secondary, by `PI`) > derived > inferred > default**

| Stratum | Change vs ADR-0022 r2 |
|---|---|
| S-1 quirk | unchanged; rows are per (source profile, **source**), so a browser add-on gets its own rows |
| S0 observed | candidates are all Primary and Secondary sources of the node's scope, ordered by `PI[source profile][group]` (§2.13). **Default `PI` rows list every primary before every secondary.** A secondary wins a group only where the primary is `Unknown` (never fetched or not expressible), unless a data row says otherwise (owner question D-4) |
| S1 patch, S2 user | unchanged |
| S3 derived | also reads evidence facts through deterministic rule rows (`title` attribute, `aria-describedby` text from a secondary DOM view when the primary lacks it). Fills only when the winner so far is `Unknown`, observed-empty or default (A6 fill rule, unchanged) |
| **S4 inferred (new)** | candidates are Inferred-class facts whose confidence passes the grant floor (§2.5) and whose basis is current (§2.4). Fill rule as S3, evaluated after S3: a derived value always beats an inferred one. Within S4 the order is static data per group (ADR-0002 §9.2: user label memory, then seen-labelled, then OCR, then image model), never a comparison of scores |
| R repair | unchanged; also masks inferred values on role-inapplicable groups |

`FUSE` stays a pure function of registers and static tables, so Theorem 1 (order independence)
and Lemma 2 (unique result) hold unchanged: S4 reads S0..S3 of the same node and its own
registers, and nothing reads S4 except R and presentation.

### 2.3 Identity: subjects, anchors and binding

#### 2.3.1 Subjects

```rust
// gorca-model::fact (tokio-free, no connector type, I-2)
pub enum Subject {
    Node(NodeKey),                          // minted by a Primary source of this scope
    Anchor { scope: ScopeRef, key: AnchorKey },
}
pub enum ScopeRef { App(AppId), Doc(DocRef) }  // DocRef = the doc mirror's existing document id
pub enum AnchorKey {
    Exact { space: KeySpace, id: u128 },    // same id in both sources (AccessKit node id in the AT-SPI path)
    DomId(Arc<str>),                        // HTML id, exposed by both engines as the AT-SPI "id" attribute
    Structural { path: u64, role: SemRole, name: u64 }, // hashed ancestor path + role + name
    Region { window: WindowRef, rect: Rect, capture: CaptureId }, // window coordinates (GTK4: WINDOW only)
    Content(ContentKey),                    // ADR-0002 §7.1
}
```

Both engines expose the HTML `id` as an AT-SPI attribute (Chromium
`ax_platform_node_base.cc:1447-1450`, Firefox `accessible/generic/LocalAccessible.cpp:1248`), so
`DomId` is the first cross-source key for the browser bus. Exact keys (ADR-0001 IR-4) remain
TBD-S3 for most elements; that research is the first task of M23.

#### 2.3.2 Binding

The node writer owns one `AnchorIndex` per scope (indices only for the anchor kinds some active
source uses: `DomId` index over the mirror's `id` attribute, an R-tree over window extents for
regions, a content-key map). A source's facts are stored **keyed by its anchor**, not by the node;
`FUSE` reads them through the current binding. Binding is therefore a function of the current
mirror and the anchor (no history), and re-binding after a re-render needs no source round trip.

```rust
pub enum BindQuality { Exact, Unique, Heuristic, Geometric, Unbound }
```

| Anchor | Quality when it resolves |
|---|---|
| `Node`, `Exact` | `Exact` |
| `DomId` | `Unique` if exactly one live node has the id, else `Heuristic` (HTML ids are not guaranteed unique) |
| `Structural` | `Heuristic` (68–88 % recall, math §3.2) |
| `Region` | `Geometric` (largest overlap ≥ 0.8 of the smaller box, one candidate) |
| none | `Unbound`: parked (bounded per source, TTL 5 s), retried when the index changes, then dropped and counted |

Each grant fixes a minimum binding quality per group class (§2.5). Defaults: role/state/name
groups need `Unique`; evidence groups accept `Heuristic`; `visual.*` evidence accepts `Geometric`.
A fact below its minimum never wins (IR-3 analogue: never speak about the wrong node); it stays in
the register for `explain`.

#### 2.3.3 Two primaries on one app

ADR-0022 §2.2 A8 is unchanged: per-app connector dedupe by default, node-level fusion only on
`Exact` keys. Structural matching between two primaries is diagnostics only.

#### 2.3.4 Synthetic nodes (reserved now, built in M24)

An Inferred source may report content that has no accessible node (text in a canvas, an unlabeled
image region). The writer may then create a **synthetic node**: a `LocalId` in a reserved key space
of the scope, parented under the smallest real node covering its region, flagged
`SemFlags::SYNTHETIC`, existence owned by the source's fact (TTL and basis, §2.4). Synthetic nodes
appear in review, flat review and browse reading; they are never focus targets, never get native
actions, and are announced with the inferred hedge. M22a reserves the key space and flag only.

### 2.4 Freshness: sequence, basis, TTL

Every register keeps `(seq, conf, basis, admitted_at)`:

- **seq** orders facts within one source only (Lemma 1); sources never compare clocks.
- **basis** records what the fact was computed from:
  `Basis::{None, VisRev(u32), DocRev(u64), Capture(CaptureId, u64 fingerprint), SemRev(u32)}`.
  The writer keeps a per-node `vis_rev` that bumps on bounds, `showing`/`visible`, children and
  primary name changes. An Inferred fact whose basis is older than the subject's current `vis_rev`
  (or whose capture fingerprint no longer matches) is **stale**: a stale fact never wins S4 and
  never triggers a change announcement. Secondary facts carry the add-on's own document revision;
  when the primary removes the bound node, they unbind (existence is primary-owned).
- **TTL** (optional per fact, required for Inferred and Annotation classes; default 5 min): expiry is
  driven by a timer wheel in the writer, which marks the node dirty; it is a lifecycle event like a
  tombstone, so the result stays a function of the fact set plus the expiry events.
- ADR-0022 §2.3 freshness rules still hold: priority, not freshness, picks the winner (F9); a stale
  primary beats a fresh secondary and is refetched with a deadline.

### 2.5 Confidence: admit by threshold, rank by static order, hedge in speech

"Confidence-weighted" is implemented as three separate mechanisms, none of which compares scores
across sources:

1. **Every fact carries `Confidence { class, p_milli }`.** `p_milli` is the source's calibrated
   precision estimate (0..1000) for this kind of fact; Primary, Patch, User and Derived facts are
   `Asserted`. Calibration is the source's duty and is verified by its milestone's gate (ADR-0002
   §9.2: τ chosen so that precision ≥ 0.9).
2. **Admission gate.** The grant has a floor per group: a fact below it is stored (for `explain`) but
   never wins. The class is computed by the writer from `p_milli` and per-group thresholds in the
   grant, never taken from the source: `Asserted` (non-inferred), `High` (≥ 0.97), `Probable`
   (≥ 0.90), `Possible` (≥ floor).
3. **Hedge.** `SemEntry` gains `hedge: u32` (2 bits per group: winner's class). Presentation
   (M22b, `gorca-present`) speaks `Probable` as "probably X" and `Possible` as "maybe X" (catalog
   ids, ADR-0002 NC wording), plain for `High`, under a `ReportFlags` bit. Braille marks hedged text
   (M10).

Ranking between sources and layers stays the static order of §2.2 (math §1.3). **Corroboration**
(two independent inferred sources agree after normalisation → class + 1, capped at `High`) is a
commutative, order-free rule and is allowed later behind an M24 gate with measured precision; it is
not in M22a.

### 2.6 The contract: descriptor, grant, port, state machine

```rust
// gorca-model::fact
pub struct SourceId(pub u16);             // per process, never reused (like ConnectorId)
pub enum SourceClass { Primary, Secondary, Inferred, Annotation }   // Patch/User stay ADR-0022 paths
pub enum TrustTier { T0Core, T1Platform, T2FirstParty, T3Untrusted }
pub struct Fact {
    pub subject: Subject,
    pub group: GroupCode,                 // u16, append-only registry (semantic + evidence ranges)
    pub value: FactValue,                 // Absent | Value(PropValue) | Retract (back to Unknown)
    pub seq: SrcSeq,                      // per-source monotone u64
    pub conf: Confidence,
    pub basis: Basis,
    pub ttl_ms: Option<u32>,
}
pub struct FactBatch { pub source: SourceId, pub facts: SmallVec<[Fact; 8]>, pub settled: bool }

// gorca-fact-api (neutral; uses gorca-rt channels)
pub struct SourceDescriptor {
    pub name: Arc<str>, pub class: SourceClass, pub version: (u16, u16),
    pub scopes: ScopeFilter,              // which apps/docs it talks about
    pub anchors: AnchorKinds,             // which AnchorKey kinds it emits
    pub wants: InputNeeds,                // what it asks the core for (captures, text), privacy-gated
}
pub struct SourceGrant {                  // computed by the core from descriptor + manifest + user consent
    pub tier: TrustTier,
    pub semantic_groups: GroupMask, pub evidence_groups: EvidenceMask,
    pub min_bind: [BindQuality; GROUP_CLASSES], pub conf_floor: [u16; GROUP_CLASSES],
    pub rate: TokenBucket,                // facts/s and burst
    pub max_registers: u32,               // memory cap across all its registers
    pub synthetic: Option<u32>,           // max synthetic nodes (None = may not create)
    pub privacy: PrivacyGrant,            // which ADR-0002 eligibility classes it may receive
}

pub trait FactSource: Send + 'static {    // non-primary sources only (Primary = Connector/DocSource)
    fn descriptor(&self) -> SourceDescriptor;
    /// Runs on its own task. Never awaited by any actor. Facts go out through `port`;
    /// requests from the core (capture this region, describe this node) come in through `ctl`.
    fn run(self: Box<Self>, port: FactPort, ctl: SourceCtl) -> BoxFuture<'static, SourceExit>;
}
```

Primary sources keep their existing transports (ADR-0004 ingress for the app connector, doc-source
deltas for the web mirror): their events already *are* the facts and already carry the actor
timeline seq. They produce the same `SrcRecord`/`Fact` value types, normalised by one normaliser per
family (SM-4). **Two transports, one contract.**

**`FactPort` (non-primary sources).** A coalescing map keyed by `(subject, group)` that keeps only
the highest seq per key (lossless by Lemma 1, so memory is bounded by distinct keys, not by rate),
plus a token bucket. The writer drains it on its **background lane** under a per-tick admission
budget (0.5 ms), after interactive work (ADR-0001 §6.2 biased select). `offer` never blocks; it
returns `Accepted | Coalesced | Throttled | Rejected(reason)`.

**Admission** (pure, in the writer, before a register is touched): class/tier allow the group;
group in grant mask; anchor kind declared; binding quality ≥ minimum (or parked); register cap not
exceeded; privacy class of the subject allows this source (X-1..X-11: no fact about a secret node is
accepted from or sent to a non-primary source); value well formed (strings ≤ 1 KiB, codes
registered). Violations are counted per reason and feed the state machine.

**Source state machine** (per source instance, owned by the source registry in `gorca-core`):

| State \ Event | `Hello` ok | `Violation` | `RateExceeded` | quiet 10 s | `TransportClosed` | `UserRevoke` / `PrivacyPause` | `Resume` |
|---|---|---|---|---|---|---|---|
| `Registered` | → `Active` | → `Revoked` | — | — | → `Detached` | → `Revoked` | — |
| `Active` | — | strike; 3 in 60 s → `Quarantined` | → `Throttled` | — | → `Detached` | → `Revoked` / `Paused` | — |
| `Throttled` (port drops beyond bucket, counts) | — | as Active | stay | → `Active` | → `Detached` | → `Revoked` / `Paused` | — |
| `Paused` (facts masked, port closed) | — | — | — | — | — | → `Revoked` | → `Active` |
| `Detached` (facts stale; purged after 30 s grace unless re-attached with the same source epoch) | → `Active` | — | — | — | — | → `Revoked` | — |
| `Quarantined` (facts masked; one notice; user may re-enable) | — | — | — | — | — | → `Revoked` | → `Active` (user) |
| `Revoked` (terminal) | — | — | — | — | — | — | — |

**Revocation is O(1).** Each source has a `SourceEpoch` (`AtomicU64`). Masking, quarantine and
revocation bump it; registers written under an older epoch read as `Unknown` immediately; the writer
re-fuses affected nodes in background chunks, focus chain first (same mechanism as the patch-set
epoch, ADR-0022 §2.9), and purges the side table lazily.

### 2.7 Trust and privacy

- **TP-1 No content leaves the machine.** No source transport uses the network. The browser bus is a
  native-messaging host binary that relays to gorca over a private `AF_UNIX` socket (0600,
  `SO_PEERCRED` same uid, as ADR-0002 §4.1). Inference runs only in the `gorca-perceive` sidecar
  (seccomp/Landlock, no network, ADR-0002 §4.6). Logs and traces carry no content beyond a 4-byte
  key prefix (PR-3).
- **TP-2 Opt-in.** Every non-primary, non-user source is off until the user enables it: per browser
  for the bus (and per site deny list), globally and per app for inference (`perception.*` in
  ADR-0002), per add-on for extension facts (manifest permission `contribute-facts`, §2.10).
- **TP-3 Privacy gate before send.** Anything the core sends to a source (a capture, node text for
  summarisation) passes `PrivacyPolicy::eligibility` first (ADR-0002 PR-1, X-1..X-11, X-4 masking
  of secret boxes). PR-0 still holds: output is never filtered.
- **TP-4 Untrusted by default.** Secondary and third-party sources are T3: page script can shape
  what a content script sees (ADR-0001 IR-8). They cannot create, hide or keep alive real nodes,
  cannot act, cannot write `existence`, and by default win only `Unknown` groups (§2.2).
- **TP-5 Bounded.** Each source is bounded in rate, registers, parked anchors and synthetic nodes;
  a flood degrades only that source (`Throttled`), never the writer's interactive lane.
- **TP-6 Model output is not instructions.** Annotation text (summaries) is spoken as content; it
  never becomes a command, a setting or an add-on input. Prompt-injection in page text can only
  change what is said, inside a labelled summary.

### 2.8 Latency rules (key → speech)

- **LB-1** The key → speech path reads only the published `SemEntry` and never awaits a non-primary
  source. Primary fetches on that path keep their deadlines (M-6b).
- **LB-2** Non-primary facts are admitted on the writer's background lane under the admission
  budget; they never pre-empt event ingestion of the focused app.
- **LB-3** On-demand requests to a source (OCR of the focused unlabeled control, NC-1) are issued
  **after** speech start, carry the current epoch, and are cancelled by `CancelBefore(epoch)`.
- **LB-4 Late facts always update the model; they change speech only by policy** (`RefinePolicy`
  rows): a late fact that fills a group the current focus utterance omitted (`may_refine`, ADR-0022
  §2.3) may append one refine tail if the epoch is unchanged and it arrives within the group's
  window (Name 500 ms per NC-1; other groups 100 ms per M-6c). Otherwise it is silent and used on
  the next visit.
- **LB-5** Inferred and Secondary winners never produce state-change utterances (ST rules diff only
  groups whose winner is Observed-primary, Patch or User). A provenance-only or hedge-only change is
  silent (F6).

### 2.9 Conflict resolution summary

| Situation | Rule | Diagnostic |
|---|---|---|
| primary vs secondary disagree, both known | `PI` order (primary first by default) | `observed-disagree(app, g, pair)`, rate-limited, evidence for `PI`/quirk rows |
| secondary fills a group the primary cannot express | secondary wins (primary `Unknown`) | provenance `ObservedSecondary` |
| inferred vs observed non-empty | observed wins | none (intent) |
| inferred vs derived | derived wins | none |
| two inferred sources | static S4 order per group | `inferred-disagree`, counted per model pair |
| inferred below floor / stale / weak binding | never wins | trail status `low-confidence` / `stale` / `weak-binding` |
| patch or user vs any source | ADR-0022 order | trail |
| same source, same seq, different value | `vkey` | `seq-collision` error (connector bug) |

### 2.10 Extensions as contributors (WIT 0.9, M18a)

- **Patches stay S1** (ADR-0022 §2.9). They are deterministic author intent and outrank observation.
- **New import interface `facts`** in the `app-module` and `site` worlds, applied through the effects
  buffer after a successful return (ADR-0019 I-3, ≤ 32 effects per call):
  `assert(subject: node-ref, group: u16, value: prop-value, conf: u16, basis: basis-ref, ttl-ms: option<u32>)`,
  `retract(subject: node-ref, group: u16)`. The host assigns the seq and the `SourceId` per
  (add-on, scope). Default class **Inferred** (T3, hedged); class **Secondary** only for add-ons the
  manifest marks first-party/reviewed (Phase 4 signing). Manifest permission
  `contribute-facts { groups, class }`; the grant is computed by the core, not declared by the
  add-on.
- **Read side:** besides ADR-0022 §2.9 accessors, `prov: func(n: node-ref, group: u16) -> option<prov-info>`
  (`layer`, `class`, `hedge`), so an add-on can tell inferred from observed.
- **Registries** (`GroupCode` semantic and evidence ranges, `SemRole`, `Field`) are produced by M22a
  as data in `gorca-ext-api`, frozen append-only by M18a (V-3).
- A **`fact-source` world** (a long-running WASM source fed by `request/deliver`) is not in 0.9; it
  ships behind `@unstable(feature = fact-source)` (V-6) if M24 needs it.

### 2.11 Invariants

- **FS-1** The core imports no source type; sources link `gorca-model::fact` and `gorca-fact-api`
  only (enforced by `cargo xtask deps`).
- **FS-2** No source writes effective semantics; every fact reaches a register in the node's writer
  and only `FUSE` combines them (SM-8).
- **FS-3** Existence of real nodes belongs to Primary sources only.
- **FS-4** Confidence gates admission and sets the hedge; it never ranks facts of different sources.
- **FS-5** An inferred fact never outlives its basis: stale facts never win and never speak.
- **FS-6** No non-primary source is awaited on the key → speech path (LB-1).
- **FS-7** Masking, quarantine and revocation of a source are O(1) on the writer turn and complete
  (the result equals a run without that source's facts, F14).
- **FS-8** Nothing excluded by ADR-0002 X-1..X-11 is sent to or accepted from a non-primary source.
- **FS-9** Every fact is explainable: `explain` lists source id, class, tier, seq, confidence class,
  basis status and binding quality.
- **FS-10** Every source is bounded in rate, registers, parked anchors and synthetic nodes.

### 2.12 Representation and memory

- `ProvTag` stays `u64`: each group's 4 bits index a fixed `ProvCode` table (layer × source class:
  `Default, Derived, Inferred, ObservedPrimary, ObservedSecondary, Patch, User, Fallback`, room for 8
  more). The instance (which add-on, which model) is recovered by `explain`.
- `SemEntry` gains `hedge: u32`. M-9 is re-measured; if `SemEntry` exceeds 112 B, `known`, `stale`,
  `fallback` and `hedge` pack into one `u128`.
- Primary registers stay inline per node (ADR-0022). Secondary, Inferred and evidence registers live
  in a **sparse side table** per writer keyed by (source, anchor), only for nodes that have them, and
  count against each source's `max_registers` and ADR-0022 M-9.

### 2.13 Profiles (ADR-0016, M14a) and the model: two planes, one trigger matcher

Per-app and per-site variability (owner, 2026-10-19) lives in the profiles system: the typed layer
stack L0..L8 with App/Window/Context/Site triggers (`docs/plan/phase-3-features.md` §3). The
semantic model and the profiles are separate planes with fixed crossing points:

| Plane | Input | Output | Owner | Changes when |
|---|---|---|---|---|
| **Fact plane** (this ADR, ADR-0022) | facts from sources, patches, user per-node overrides | `SemEntry` per node | node writer (`FUSE`) | facts arrive |
| **Settings plane** (ADR-0016) | layer files L0..L7, triggers | `Arc<Settings>` per `ProfileKey` | Reader (single writer, PM-3) | focus/window/doc changes, edits |

- **PS-1 Profiles never write facts, facts never write settings.** Settings gate presentation
  (`ReportFlags`, verbosity, hedge wording) and policy (mode defaults); they are not fused (ADR-0022
  A7) and stay in the presentation cache key.
- **PS-2 Mode defaults belong to profiles.** Keys `browse.mode_default` (`browse | focus`),
  `browse.auto_focus_mode` (`widgets | never`), `browse.sticky_focus` (bool) and `browse.enabled`
  (bool) are profile keys (`vendor_profileable`). The Reader's mode machine owns the *decision*:
  `mode = policy(settings.browse.*, document semantics, user toggle)`. Document semantics (role
  `application`, `aria-busy`, freshness) are model inputs to that policy, never settings. The user's
  runtime toggle (DP-3 sticky focus) is session state; "save mode as default for this app/site" is
  a PE-1 edit into the App or Site profile.
- **PS-3 Add-ons contribute profile rows as data** (vendor profiles, L4, PF-1..PF-4), not hooks. The
  CHR-0 interim per-app mode table must use the PS-2 key names and the `AppMatcher` semantics so its
  rows move verbatim into a first-party vendor profile (`ext-chromium`, e.g. `code` →
  `browse.enabled = false`) when M14a lands. An add-on never activates a profile (PF-3). Runtime
  mode requests stay the M18a `request-mode` effect, below the user's toggle.
- **PS-4 Triggers read the model, not the bus.** `Context` triggers match the focus's published
  `SemEntry` role class (so the profile and the speech agree on what the focus is); `Site` triggers
  read `DocMeta.url`. Later, add-on *context tags* (PF-3, Phase 4) are evidence facts on the document
  node (`ctx.tag`) that `Context` triggers match: one mechanism, no profile hook.
- **PS-5 Source enablement is settings, grants are computed per scope.** Opt-ins (TP-2:
  `sources.browser_bus`, `perception.*`, per-add-on `contribute-facts`) may be set in L3, L5 and L6
  for an App or Site trigger, **never by vendor profiles (L4)**: an add-on cannot enable a data flow.
  The core folds them per scope (App/Site triggers only, not Window/Context, so a grant does not flap
  with focus) into the source's `SourceGrant`; a change bumps that source's epoch (§2.6).
- **PS-6 User semantic overrides are not profile keys.** Per-node overrides and "label this control"
  are S2 user facts (label memory key, NC-2). User *rules* with selectors ("on this site, `.icon-x`
  is a Close button") are user-authored patches in the S2 layer; their activation reuses the
  profiles' `Trigger` type and matcher (`gorca-config`), so "per site" means the same thing in both
  planes.
- **Terminology.** ADR-0022's `PI[app profile]` and quirk rows are keyed by a **source profile**
  (engine/toolkit class: `gecko`, `blink`, `gtk4`, `electron`, ...), selected by `AppMatcher` from
  first-party or add-on data. It is not a user profile and users do not edit it through ADR-0016.

### 2.14 What M22a implements and what comes later

| Part | M22a | Later |
|---|---|---|
| Value types (`Fact`, `Subject`, `AnchorKey`, `Confidence`, `Basis`, `BindQuality`, `GroupCode` ranges, `ProvCode`) | **yes** | — |
| `SourceDescriptor`, `SourceGrant`, admission, `FactPort`, state machine, source epochs | **yes** | — |
| Strata S-1..R including S4 and the evidence side table, hedge mask | **yes** (exercised by the fake source) | rule rows for real evidence: M23/M24 |
| Binding: `Node` and `Exact` anchors | **yes** | `DomId` M23, `Structural` M23, `Region`/`Content` M24 |
| AT-SPI primary source through `gorca-sem-atspi` | **yes** | — |
| `FakeFactSource` + conformance kit (any class, scripted floods, revocation, bad facts) | **yes** | reused by every later source |
| Synthetic nodes | key space and flag reserved | M24 |
| AccessKit/Newton primary | — | M19 |
| Browser add-on bus (WebExtension, native host, `gorca-source-webext`) | — | M23 |
| Inference source (OCR naming cascade, image/UI description) | — | M24 |
| Annotations (summaries, long descriptions) | type reserved | M25 |
| WIT `facts` interface, `prov` accessor | registries only | M18a |
| Hedge wording in speech, refine policy rows | `hedge` stored | M22b1 (compose), M24 (policy rows) |

## 3. Alternatives considered

- **A. Numeric weighted fusion across all sources** (weighted vote, log-odds or Dempster-Shafer over
  per-fact confidences). Literal reading of "confidence-weighted". Rejected for ranking: scores of
  AT-SPI, a WebExtension and a vision model are on different, uncalibrated scales; the result is
  not explainable ("0.82 > 0.79"); a confident wrong model could override an author's label; and
  per-group trust cannot be expressed by one global scale (math §1.3). Kept in a safe form:
  threshold admission, hedge, and later order-free corroboration inside S4 (§2.5). Owner question
  D-1.
- **B. Browser add-on as a replacement doc source** (ADR-0001 TBD-S2, `SourceKind::WebExt`) instead
  of a fact contributor. Still possible through the `DocSource` trait for bulk snapshots, but the
  primary must stay AT-SPI for focus, caret and actions (TBD-S4, IR-5). A fact contributor gives the
  richer DOM facts without moving the primary. Chosen first.
- **C. Inferred values spliced into the name** (Chromium image annotations). Rejected: provenance
  and hedging lost, status strings become names, caches and ST rules see model churn as changes.
- **D. A separate result document for model output** (NVDA content recognition). Kept for
  user-invoked full-window OCR (a review surface, M24), rejected as the only path: it cannot name an
  unlabeled button in place.
- **E. Facts keyed by node instead of anchor.** Simpler, but every re-render or rebuild would orphan
  secondary facts and need a source round trip. Rejected.
- **F. One generic transport for primary sources too.** Rejected for M22a: AT-SPI events already
  carry the actor timeline seq through a tuned coalescer; moving them behind `FactPort` buys nothing
  and risks K2.

## 4. Consequences

- New crates `gorca-fact-api` (neutral) and `gorca-fuse` (neutral, tokio-free) beside
  `gorca-sem-atspi`; `gorca-model` gains `sem` and `fact` modules.
- Every future source is "implement `FactSource`, pass the conformance kit, get a grant"; no core
  change per source.
- `explain` answers "why did gorca say *probably Send*": OCR fact, model version, confidence class,
  capture basis, binding quality.
- The writer carries an anchor index and a side table only when a non-primary source is active;
  with only AT-SPI the cost is one empty-map check per fused node.
- ADR-0002's naming cascade becomes S4 rule data rather than a feature-store side path.
- ADR-0001 TBD-S3 (join keys per browser) becomes the gating research of M23.
- Owner questions D-1..D-6 (kickoff report §6) shape defaults, not the contract.

## 5. Metrics to validate

| Id | Metric | Gate | When |
|---|---|---|---|
| M-14 | Admission cost, fake Secondary source at 5k facts/s beside the B-FUSE mix | ≤ 1 µs/fact p99; K2 key → speech Δp99 ≤ 0.1 ms; ingress lag unchanged | M22a |
| M-15 | Flood: one fake source at 100k facts/s for 60 s | its registers ≤ `max_registers`; writer RSS growth ≤ cap; `Throttled` within 1 s; other sources unaffected | M22a |
| M-16 | Revocation of a source holding 100k registers | writer turn ≤ 50 µs; effective state equals the run without it (F14) within 1 s | M22a |
| M-17 | Properties F11–F17 (kickoff report §4) | all pass in `cargo test` | M22a |
| M-18 | Binding precision on the M8 fixture pages (`DomId` + `Structural`) | ≥ 99 % of `Unique` bindings correct; 0 facts written through `Heuristic` to role/state/name | M23 |
| M-19 | Inferred name precision (OCR + image model) on the unlabeled-control corpus | ≥ 0.90 for `Probable`, ≥ 0.97 for `High` | M24 |
| M-20 | Late-fact refine | ≤ 1 refine tail per focus; 0 refinements after an epoch change | M22b1 + M24 |

## 6. Open questions

Owner questions D-1..D-6 are in the kickoff report §6 with recommendations. Engineering unknowns:
exact cross-source keys per browser (TBD-S3, M23); capture → window-id mapping and capture latency
through the gorca-helper `CaptureWindowArea` (ADR-0002 §10, M24); calibration data for the models
(M24); Newton per-tree ordering (ADR-0022 QO-4).
