# ADR-0024: Fact sources: taxonomy, contract, identity, trust and confidence for the semantic model

Status: **Proposed** (r2, 2026-10-19; r1 reviewed in `docs/reports/adr-0024-review.md`,
ACCEPT-WITH-CHANGES, all findings integrated, §8 maps them). Owner: architect. Amends ADR-0022 r2
§2.1 (entry layout, frozen groups) and §2.2 (layers, source slots); ADR-0022 r3 points here.

- Kickoff analysis, prior art, worked examples, owner questions: `docs/reports/m22a-kickoff.md`.
- Implementation plan for the parts M22a builds: `docs/plan/m22a-spec.md`.
- Builds on: ADR-0022 r2 (registers, `FUSE`, `SemEntry`, `ProvTag`, `explain`, SM-1..SM-8),
  `docs/reports/semantic-fusion-math.md` (Lemma 1–3, Theorem 1, F1–F10), ADR-0001 §4.1 (identity
  I-1..I-3), §6.2 (channels), §9.3 (IR-1..IR-8, TBD-S1..S4), ADR-0002 (perception sidecar, privacy
  PR-0..PR-5, exclusions X-1..X-11, naming cascade NC-1..NC-3), ADR-0003 (doc mirror), ADR-0004
  (ingress coalescer), ADR-0019 (worlds, effects buffer, I-1..I-8, V-1..V-8, permissions),
  ADR-0016 (profiles, **not yet written**; design in `docs/plan/phase-3-features.md` §3).
- Milestones: M22a (contract types, pure fusion with all strata, AT-SPI source, fake source),
  **M23-0** (source runtime: `FactPort`, registry, flood benches), M19 (AccessKit/Newton), M18a
  (WIT 0.9 `facts`), **M23** (browser add-on bus), **M24** (local inference facts), **M25**
  (annotations). `docs/plan/phase-3.md` §M22, §M23–M25.

r2 changes: frozen 16 fused groups and a 104 B entry (H-1, H-2); secondaries win only statically
inexpressible groups (H-3); transport-pinned scopes (H-4, FS-11); taint through derivation (H-5);
binding generations and two-sided id uniqueness (H-6); normaliser placement (H-7); runtime moved to
M23-0 (M-3); admission slicing, port caps, per-scope strikes, stable-identity quarantine,
`SourceId` widening, S4 tie-break, immediate silencing on revoke, per-writer ports, source-side
privacy (M-4..M-12); WIT shapes (W-1..W-4); profiles interaction kept, D-6 conditional on ADR-0016.

## 1. Context

The owner decided on 2026-10-19 that Phase 3 starts with M22a and that the semantic model's source
contract must be open to future fact sources, all of them provenance-tagged and confidence-weighted
contributors to one model. Only the contract is designed in M22a; the sources are later milestones.

| Source | What it knows | Trust | Arrives |
|---|---|---|---|
| AT-SPI2 (today) | the toolkit's or engine's own accessibility tree | platform | events + deadline-bounded fetches |
| Newton / AccessKit (M19) | the same, Wayland-native, whole-tree updates | platform | tree updates per surface |
| Browser add-on bus (M23): Firefox/Chrome WebExtension over native messaging relayed to a gorca socket | DOM, ARIA source attributes, CSS visibility, layout boxes, icon classes, `src` names, shadow DOM | page-influenced, untrusted | pushed mutation batches |
| gorca WASM extensions (M18a) | app- or site-specific knowledge | add-on: first-party or third-party | effects after a hook returns |
| Local ML inference (M24/M25): OCR of in-memory captures from the gorca-helper shell extension, image/icon/UI-element description, summarisation | pixels and text interpreted by a model | model output, calibrated per model | seconds later, on demand or in bulk |

ADR-0022 r2 already has the core: registers per (node, source slot, group), a pure `FUSE` run by
the node's single writer (SM-8), a static layer order and trust table `PI`, `Unknown` vs `Absent`,
provenance beside the value, `explain`. Its gaps for this list: a closed slot list; every subject
is a `NodeKey` minted by the primary; no confidence, no evidence that needs interpretation, no
staleness tied to what the screen shows; every source trusted like AT-SPI and unbounded; no rule
for late facts on the key → speech path.

Prior art (details in the kickoff report §2): Chromium splices image annotations into the name,
status strings included (`refs/chromium/ui/accessibility/platform/ax_platform_node_base.cc:235-262`);
NVDA keeps OCR in a separate user-invoked document (`refs/nvda/source/contentRecog/recogUi.py:95,291`);
ADR-0002 §9.2 already has a hedged, calibrated cascade; the math report rejects numeric confidence
for ranking across sources (§1.3; ADR-0022 alternative G).

## 2. Decision

### 2.1 Source taxonomy

Every contributor is a **fact source**. A source belongs to exactly one class; the class fixes the
highest layer it can write, what it may create, and its transport.

| Class | Examples | Highest layer | May own existence | Subjects | Transport | Default tier |
|---|---|---|---|---|---|---|
| **Primary** | AT-SPI connector + doc source, AccessKit/Newton connector | Observed (S0) | yes, real nodes | `NodeKey` it mints | in-band with events (ADR-0004 ingress, doc-source deltas) | T1 platform |
| **Secondary** | browser add-on bus | Observed (S0), only for statically inexpressible groups (§2.2) | no | anchors (§2.3) | `FactPort` (§2.6) | T3 untrusted |
| **Patch** | add-on patches (ADR-0019 PA-1) | Patch (S1) | no (only `hide`) | selector matches | ext host, epoch | T2/T3 by manifest |
| **User** | overrides, label-this-control (NC-2), user rules | User (S2) | no | `NodeKey` / label memory key / selector | settings | T0 |
| **Inferred** | OCR, icon/image/UI-element models, label memory "seen elsewhere", extension facts (§2.10) | Inferred (S4, below Derived) | **synthetic nodes only** (§2.3.5, M24) | anchors incl. regions | `FactPort` | T2 (gorca sidecar) / T3 (extension) |
| **Annotation** | summaries, long image descriptions | none: never fused | no | subtree or region | `FactPort`, on demand | as producer |

Annotations are **content**, spoken only by an explicit command (or a user-enabled rule) as declared
content utterances (ADR-0022 §2.9); never part of a `SemEntry`, a name or a cache key.

A fact has a **kind**:

- **Semantic fact**: a value for one of the 16 fused groups (§2.12).
- **Evidence fact**: an input that rules must interpret (`hint.icon_class`, `hint.src_name`,
  `hint.title_attr`, `visual.ocr_text`, `visual.label_guess`, `layout.box`, `layout.css_hidden`).
  Evidence lives in a sparse side table, is never presented directly, and is read only by the
  Derived stratum S3 and the Inferred stratum S4. Group codes with bit 15 set are evidence (W-3).

### 2.2 Layers and strata (amends ADR-0022 §2.2)

Precedence per group, highest first:

**user > patch > observed (primary; secondary only where statically allowed) > derived > inferred > default**

| Stratum | Rule |
|---|---|
| S-1 quirk | rows per (source profile, source); a browser add-on gets its own rows |
| S0 observed | candidates are the Primary sources of the node's scope, ordered by `PI[source profile][group]`. **A Secondary is a candidate only for groups the source profile's static `inexpressible` row lists for that primary** (H-3; e.g. a hypothetical `layout.css_hidden`-derived flag a given engine never exports). A dynamic `Unknown` of the primary (not fetched yet, partial focus mask) **never** admits a secondary. `inexpressible` rows are first-party data with a decision row each (D-4) |
| S1 patch, S2 user | unchanged (S2 also holds user selector rules, §2.13 PS-6) |
| S3 derived | deterministic rules over the node's final values, children, relation targets and evidence (`title` names an unnamed control, `aria-describedby` text). Fills only when the winner so far is `Unknown`, observed-empty or default (A6 fill rule) |
| **S4 inferred** | Inferred-class facts that pass the grant's confidence floor (§2.5), are not stale (§2.4) and are bound well enough (§2.3). Fill rule as S3, evaluated after S3. Ranking inside S4: `(ConfClass desc, kind order, ext_id or model id)`, all static or deterministic per fact (M-9, L-5) |
| R repair | unchanged; also masks inferred values on role-inapplicable groups |

**Taint (H-5).** Every value produced by S3 or S4 carries the lowest trust tier and the lowest
confidence class among its inputs. A Derived value computed from T3 evidence gets
`ProvCode::DerivedTainted` and the hedge of its weakest input, where any T3 input caps the class at
`Probable` (so "probably Delete", never a plain T3-derived name), and is treated like an Inferred winner
by LB-5, by the act-on-what-was-spoken check (§2.3.3) and by `explain`. A Derived value from
primary inputs only stays `Derived`/`Asserted`.

**Class transitions are silent (H-3).** A change of the winning class or provenance at an equal
value, and any transition whose new winner is not Observed-primary, Patch or User, produces no
state-change utterance (extends ADR-0022 F6 and LB-5).

`FUSE` stays a pure function of registers, static tables and an **epoch snapshot passed in by the
writer** (M-10: no atomic is read inside `FUSE`), so Theorem 1 and Lemma 2 hold unchanged.

### 2.3 Identity: subjects, anchors and binding

#### 2.3.1 Subjects

```rust
// gorca-model::fact (tokio-free, no connector type, I-2)
pub enum Subject {
    Node(NodeKey),                          // minted by a Primary source of this scope
    Anchor(AnchorKey),                      // scope is NOT in the fact: pinned by the transport (§2.3.4)
}
pub enum AnchorKey {
    Exact { space: KeySpace, id: u128 },    // same id in both sources (AccessKit node id in the AT-SPI path)
    DomId { frame: FrameKey, shadow: u64, id: Arc<str>, dom_count: u16 }, // §2.3.2
    Structural { path: u64, role: SemRole, name: u64 },  // hashed ancestor path + role + name
    Region { window: WindowRef, rect: Rect, capture: CaptureId }, // window coordinates (GTK4: WINDOW only)
    Content(ContentKey),                    // ADR-0002 §7.1
}
```

Both engines expose the HTML `id` as an AT-SPI attribute (Chromium
`ax_platform_node_base.cc:1447-1450`, Firefox `accessible/generic/LocalAccessible.cpp:1248`).

#### 2.3.2 Binding

The node writer owns an `AnchorIndex` per scope, with indices only for anchor kinds an active source
uses. Facts are stored keyed by `(source, anchor)`; `FUSE` reads them through the binding.

```rust
pub enum BindQuality { Exact, Unique, Heuristic, Geometric, Unbound }
pub struct Binding { pub node: NodeKey, pub quality: BindQuality, pub gen: BindGen }
```

| Anchor | Quality |
|---|---|
| `Node`, `Exact` | `Exact` |
| `DomId` | `Unique` only if **both sides** agree: the add-on reports `dom_count == 1` for the id within its frame and shadow root (hidden templates and `aria-hidden` copies included), and exactly one live mirror node in the same `DocRef`/frame has it. Otherwise `Heuristic` |
| `Structural` | `Heuristic` (68–88 % recall, math §3.2) |
| `Region` | `Geometric` (largest overlap ≥ 0.8 of the smaller box, one candidate) |
| none | `Unbound`: parked (≤ 256 per source and scope, TTL 5 s), retried on index change, then dropped and counted |

**Binding generations (H-6).** Each binding has a `BindGen`, bumped whenever the anchor resolves to
a different node or the bound node is removed. A register stores the `BindGen` it was asserted
against. A fact whose stored generation differs from the current binding is **stale**: it never
wins until the source re-asserts it with a newer document revision. So a `DomId` reused by a new
element after an SPA re-render, a virtualised list recycle or a CHR-0 rebuild never inherits the old
element's facts.

Each grant fixes a minimum binding quality per group class: name/description and all semantic
groups need `Unique`; evidence accepts `Heuristic`; `visual.*` evidence accepts `Geometric`. A fact
below its minimum never wins (IR-3 analogue) and stays in the register for `explain`.

#### 2.3.3 Acting on what was spoken

CHR-0's "act only on what the user heard" check on Enter (`chromium-base-design-review.md` §3)
refuses the action, with one spoken notice, when the spoken name's winner was non-primary or tainted
and its `BindGen` or the node's `sem_rev` changed after the utterance (H-6, UI-redress guard).

#### 2.3.4 Scope is pinned by the transport (H-4, FS-11)

A source never declares the scope of a fact. The core derives it from the connection:

- **Browser bus:** the native host is spawned by the browser. On connect, the core reads the peer pid
  (`SO_PEERCRED`), walks the process ancestry to the browser process, and maps that to the browser's
  `AppId` through the connector's pid table. The connection is bound to that app only. Each message
  names a `(tab, frame)`; the core maps it to a `DocRef` of that app and checks it against
  `DocMeta.url` (the add-on sends the URL with each document hello). A fact for any other app or a
  document that does not match is a `Violation` for that scope. A same-uid process that is not a
  descendant of a known browser process gets no scope at all. Flatpak/Snap browsers: verified in
  M23's first task; until then they are unsupported.
- **WASM extensions:** the scope is the world instance's scope (app-module: its app; site: its
  document).
- **Inference sidecar:** the scope is the scope of the request the core sent (every inferred fact
  answers a core request; unsolicited facts are violations).

#### 2.3.5 Two primaries on one app; synthetic nodes

ADR-0022 §2.2 A8 unchanged (per-app dedupe, node fusion only on `Exact` keys).

Synthetic nodes (reserved now, built in M24): an Inferred source may report content with no
accessible node (text in a canvas). The writer may create a node in a reserved `LocalId` key space,
parented under the smallest real node covering its region, flagged `SemFlags::SYNTHETIC`, existence
owned by the fact (TTL, basis). Never a focus target, no native actions, always hedged. M22a reserves
the key space and the flag only.

### 2.4 Freshness: sequence, basis, generation, TTL

Every non-primary register keeps `(seq, conf, basis, bind_gen, admitted_at)`:

- **seq** orders facts within one source only (Lemma 1).
- **basis**: `Basis::{None, VisRev(u32), DocRev(u64), Capture(CaptureId, u64 fingerprint), Snapshot(SnapshotToken)}`.
  The writer keeps a per-node `vis_rev` that bumps on bounds, `showing`/`visible`, children and
  primary name changes. An Inferred fact older than the subject's `vis_rev` (or with a non-matching
  capture fingerprint) is stale. Secondary facts carry the add-on's document revision; together with
  `BindGen` (§2.3.2) this makes **every** non-primary class subject to staleness, not only Inferred.
  `Snapshot` is the host-issued token for extension facts (§2.10, W-4).
- **TTL** (required for Inferred and Annotation, default 5 min; optional for Secondary): a timer wheel
  in the writer marks the node dirty at expiry; expiry is a lifecycle event like a tombstone.
- A stale fact never wins and never speaks. ADR-0022 F9 holds: priority, not freshness, picks among
  primaries; a stale primary beats a fresh secondary.

### 2.5 Confidence: admit by threshold, rank by class then static order, hedge in speech

1. **Every fact carries `Confidence { class, p_milli }`.** `p_milli` is the source's calibrated
   precision estimate (0..1000). Primary, Patch, User and untainted Derived facts are `Asserted`.
   Calibration is verified by the source's milestone gate (ADR-0002 §9.2: precision ≥ 0.9).
2. **Admission.** The grant has a floor per group class. The writer computes the class from
   `p_milli` and the grant's thresholds (never taken from the source): `Asserted`, `High` (≥ 0.97),
   `Probable` (≥ 0.90), `Possible` (≥ floor). Below the floor: stored for `explain`, never wins.
3. **Ranking.** Across layers and across primaries: the static order of §2.2. Inside S4 only:
   `(ConfClass desc, kind order, id)`. The class is a deterministic function of the fact and the
   grant, so this is order-independent; raw `p_milli` values are never compared.
4. **Hedge.** `SemEntry.hedge` (2 bits per group) holds the winner's class (or the taint class).
   Presentation (M22b1) speaks `Probable` as "probably X" and `Possible` as "maybe X" (catalog ids,
   ADR-0002 wording), plain for `High` and `Asserted`, under a `ReportFlags` bit; braille marks
   hedged text (M10).

Corroboration (two independent inferred sources agree → class + 1, capped at `High`) is order-free
and may be added behind an M24 gate with measured precision; not in M22a.

### 2.6 The contract: descriptor, grant, admission, port, state machine

```rust
// gorca-model::fact
pub struct SourceId { pub slot: u32, pub gen: u32 }   // u32 slot, recycled with a generation (M-8)
pub enum SourceClass { Primary, Secondary, Inferred, Annotation }   // stored as u8 codes
pub enum TrustTier { T0Core, T1Platform, T2FirstParty, T3Untrusted }
pub struct Fact {
    pub subject: Subject,
    pub group: GroupCode,                 // u16; bit 15 = evidence
    pub value: FactValue,                 // Absent | Value(PropValue) | Retract (back to Unknown)
    pub seq: SrcSeq,                      // per-source monotone u64
    pub conf: Confidence,
    pub basis: Basis,
    pub ttl_ms: Option<u32>,
}

// gorca-fact-api (neutral; contract types now, runtime in M23-0)
pub struct SourceDescriptor {
    pub name: Arc<str>, pub class: SourceClass, pub version: (u16, u16),
    pub anchors: AnchorKinds, pub wants: InputNeeds,   // no scopes: pinned by transport (FS-11)
}
pub struct StableIdentity { pub name: Arc<str>, pub peer: PeerProof }  // verified exe or extension id
pub struct SourceGrant {                  // computed by the core: descriptor + manifest + settings
    pub tier: TrustTier,
    pub semantic_groups: GroupMask, pub evidence_groups: EvidenceMask,
    pub min_bind: [BindQuality; GROUP_CLASSES], pub conf_floor: [u16; GROUP_CLASSES],
    pub rate: TokenBucket,                // per (source, scope)
    pub max_registers: u32, pub max_port_entries: u32, pub max_parked: u16,
    pub synthetic: Option<u32>,
    pub privacy: PrivacyGrant,
}
pub fn admit(f: &Fact, g: &SourceGrant, scope: &ScopeView) -> Admission;   // pure
pub fn step(s: SourceState, e: SourceEvent) -> (SourceState, Vec<SourceEffect>); // pure, exhaustive

pub trait FactSource: Send + 'static {    // non-primary sources only
    fn descriptor(&self) -> SourceDescriptor;
    fn run(self: Box<Self>, port: FactPort, ctl: SourceCtl) -> BoxFuture<'static, SourceExit>;
}
```

Primary sources keep their transports (ADR-0004 ingress, doc-source deltas): their events already
carry the actor-timeline seq. They emit the same `SrcRecord`/`Fact` values through one normaliser
per family (SM-4). **Two transports, one contract.**

**`FactPort` (M23-0).** One port per (source, writer) (M-11); a router resolves the pinned scope to
the owning writer (doc actor for a `DocRef`, app actor otherwise). The port is a per-producer
coalescing map keyed by `(subject, group)` keeping the highest seq (lossless by Lemma 1), **capped at
`max_port_entries`** (derived from `max_registers`); the token bucket is applied inside `offer`.
`offer` never blocks and returns `Accepted | Coalesced | Throttled | Rejected(reason)`; reaching the
cap or the bucket moves that (source, scope) to `Throttled` (M-5).

**Admission** (pure, in the writer, before a register is touched), in order: scope pinned and
matching (FS-11); class/tier allow the group; group in grant; anchor kind declared; binding quality
≥ minimum (or parked); register cap; privacy class of the subject allows this source (FS-8); value
well formed (codes registered; **page-derived strings longer than 1 KiB are truncated and counted,
not violations**, M-6). The writer admits in **slices of ≤ 50 µs** and checks its interactive lane
between slices (M-4).

**Source state machine.** Strikes, buckets, `Throttled` and `Quarantined` are kept **per (source,
scope)** first (one hostile tab never disables the bus for other tabs, M-6); a source-wide quarantine
needs 3 scopes quarantined within 10 minutes. Quarantine and pause are keyed by `StableIdentity` for
the whole session, so reconnecting does not escape them (M-7).

| State \ Event | `Hello` ok | `Violation` | `RateExceeded` / port cap | quiet 10 s | `TransportClosed` | `UserRevoke` / `PrivacyPause` | `Resume` |
|---|---|---|---|---|---|---|---|
| `Registered` | → `Active` (or the remembered `Quarantined`/`Paused` of its `StableIdentity`) | → `Revoked` | — | — | → `Detached` | → `Revoked` / `Paused` | — |
| `Active` | — | strike; 3 in 60 s → `Quarantined` | → `Throttled` | — | → `Detached` | → `Revoked` / `Paused` | — |
| `Throttled` | — | as Active | stay | → `Active` | → `Detached` | → `Revoked` / `Paused` | — |
| `Paused` (facts masked, port closed) | — | — | — | — | purge registers; stay `Paused` | → `Revoked` | → `Active` |
| `Detached` (facts stale; purged after 30 s unless re-attached with the same `StableIdentity`) | → `Active` | — | — | — | — | → `Revoked` | — |
| `Quarantined` (facts masked; one notice) | — | — | — | — | purge registers; stay `Quarantined` | → `Revoked` | → `Active` (user only) |
| `Revoked` (terminal for this `StableIdentity` this session) | — | — | — | — | — | — | — |

**Masking and revocation (M-10).** Each (source, scope) has a `SourceEpoch`. A mask or revocation is
delivered to the writer as a **timeline message** (not an atomic read inside `FUSE`). In that same
turn the writer re-fuses **synchronously the focus chain, the active descendant and the visible browse
window** (bounded, ≤ 200 nodes, ≤ 0.5 ms) and publishes them, so a quarantined source's names are
silent immediately; the rest is re-fused in background chunks, and the side table is purged lazily.

### 2.7 Trust and privacy

- **TP-1 No content leaves the machine.** No source transport uses the network. The browser bus is a
  native-messaging host relaying to gorca over a private `AF_UNIX` socket (0600), scope-pinned as in
  §2.3.4. Inference runs only in the `gorca-perceive` sidecar (no network, ADR-0002 §4.6). Logs carry
  at most a 4-byte key prefix (PR-3).
- **TP-2 Opt-in.** Every non-primary, non-user source is off until the user enables it (per browser +
  per-site deny list for the bus; global and per app for inference; per add-on for extension facts).
- **TP-3 Privacy at the source, again in the core (M-12).** The gorca WebExtension never reads input
  values, `type=password` fields, `autocomplete=cc-*`/`current-password`/`one-time-code` fields or
  their subtrees, skips private windows and browser-internal pages (`about:`, `chrome:`, extension
  pages), and applies the X-1..X-11 rules before anything crosses the socket. The core re-checks
  `PrivacyPolicy::eligibility` on admission and before sending anything to a source (ADR-0002 PR-1,
  X-4 masking of captures). PR-0 holds: output is never filtered.
- **TP-4 Untrusted by default.** Secondary and third-party sources are T3: page script shapes what a
  content script sees (IR-8). They cannot create, hide or keep alive real nodes, cannot act, cannot
  write `existence`, win only statically inexpressible groups, and taint whatever is derived from
  them.
- **TP-5 Bounded.** Rate, registers, port entries, parked anchors and synthetic nodes per (source,
  scope); a flood degrades only that scope.
- **TP-6 Model output is not instructions.** Annotation text is spoken as labelled content; it never
  becomes a command, a setting or an add-on input.

### 2.8 Latency rules (key → speech)

- **LB-1** The key → speech path reads only published `SemEntry`s and never awaits a non-primary
  source. Primary fetches keep their deadlines (M-6b).
- **LB-2** Non-primary facts are admitted on the writer's background lane in ≤ 50 µs slices with an
  interactive-lane check between slices; never ahead of event ingestion for the focused app.
- **LB-3** On-demand requests (OCR of the focused unlabeled control, NC-1) are issued after speech
  start, carry the epoch, and are cancelled by `CancelBefore(epoch)`.
- **LB-4 Late facts always update the model; they change speech only by policy** (`RefinePolicy`
  rows): one refine tail if the group was omitted (`may_refine`), the epoch is unchanged, and the fact
  arrives within the group's window (Name 500 ms per NC-1; others 100 ms per M-6c).
- **LB-5** Inferred, Secondary and tainted winners never produce state-change utterances; class or
  provenance changes at equal value are silent (§2.2).

### 2.9 Conflict resolution summary

| Situation | Rule | Diagnostic |
|---|---|---|
| primary vs secondary on an expressible group | secondary is not a candidate | `observed-disagree` counter only (evidence for an `inexpressible` row) |
| secondary on a statically inexpressible group | secondary wins | provenance `ObservedSecondary`, hedged by tier |
| derived from T3 evidence | Derived layer, tainted | `DerivedTainted`, hedge |
| inferred vs observed non-empty or untainted derived | higher layer wins | none (intent) |
| two inferred sources | `(ConfClass, kind order, id)` | `inferred-disagree` per model pair |
| inferred below floor / stale / weak or old binding | never wins | trail status `low-confidence` / `stale` / `weak-binding` / `rebound` |
| patch or user vs any source | ADR-0022 order | trail |
| same source, same seq, different value | `vkey` | `seq-collision` error |

### 2.10 Extensions as contributors (WIT 0.9, M18a)

- **Patches stay S1** (ADR-0022 §2.9): deterministic author intent.
- **Import interface `facts`** in the `app-module` and `site` worlds, applied through the effects
  buffer after a successful return (ADR-0019 I-3, ≤ 32 effects per call):
  - `assert(target: fact-target, group: u16, value: prop-value, conf: u16, basis: snapshot-token, ttl-ms: option<u32>)`
  - `retract(target: fact-target, group: u16)`
  - `variant fact-target { node(node-ref), dom-id(dom-anchor), structural(structural-anchor), region(region-anchor), selector(selector-ref) }`
    — a variant **with all cases in 0.9** (W-1), so M23/M24 need no type change; cases a world cannot
    use fail with `unsupported` at run time.
  - `snapshot-token` (W-4) is an opaque `u64` the host passes in every hook input; the host maps it to
    the `(sem_rev, vis_rev, BindGen)` of the nodes the hook could read, so a fact computed from an
    old view is stale on arrival. No extension computes a basis.
- The host assigns seq and `SourceId` per **add-on instance** (scope lives in the register key, M-8).
  Default class Inferred (T3, hedged); Secondary only for first-party/reviewed add-ons (Phase 4
  signing). Manifest permission `contribute-facts { groups, class }`; the grant is computed by the core.
- **Read side:** `prov: func(n: node-ref, group: u16) -> option<prov-info>` where `prov-info` is a record
  of **`u8` registry codes** (`layer`, `class`, `hedge`, `taint`), not WIT enums (W-2).
- **Registries:** `GroupCode` (bit 15 = evidence, W-3), `SemRole`, `ProvCode`, `SourceClass`, `Field`,
  append-only, frozen by M18a (V-3). Single source of truth: `gorca-model`; `gorca-ext-api` carries a
  generated copy with an equality test (L-1).
- A `fact-source` world (long-running WASM source) is not in 0.9; `@unstable(feature = fact-source)`
  if M24 needs it.

### 2.11 Invariants

- **FS-1** The core imports no source type; sources link `gorca-model::fact` and `gorca-fact-api`;
  `gorca-core` never depends on a `gorca-source-*` crate (xtask rule, L-2).
- **FS-2** No source writes effective semantics; only `FUSE` in the node's writer combines facts (SM-8).
- **FS-3** Existence of real nodes belongs to Primary sources only.
- **FS-4** Raw confidence scores are never compared across sources; confidence gates admission, sets
  the hedge, and orders candidates only inside S4 by class.
- **FS-5** No non-primary fact outlives its basis or binding generation: stale facts never win or speak.
- **FS-6** No non-primary source is awaited on the key → speech path.
- **FS-7** Masking and revocation are O(1) to issue, silence the focus chain and visible window in the
  same writer turn, and are complete (F14).
- **FS-8** Nothing excluded by ADR-0002 X-1..X-11 is read by, sent to or accepted from a non-primary
  source.
- **FS-9** Every fact is explainable: source, class, tier, seq, confidence class, basis status,
  binding quality and generation, taint.
- **FS-10** Every source is bounded per (source, scope).
- **FS-11** A fact's scope comes from its transport, never from the fact.
- **FS-12** Values derived from untrusted or inferred inputs are tainted and hedged.

### 2.12 Representation, frozen groups and memory (H-1, H-2)

**Frozen fused groups (16)**, in code order: `role` (incl. `roledescription`), `name`, `description`,
`check`, `press`, `expand`, `select`, `current`, `invalid`, `popup`, `avail`, `flags` (all boolean
flags plus `orientation` and `sort`), `level`, `setpos`, `grid`, `live`. Not fused groups, outside
`ProvTag`: `existence` (lifecycle register) and `text`/value/caret (primary-only, ADR-0022 math §1.4).
A 17th group requires a layout change and an ADR amendment; WIT is unaffected (codes and accessors).
`const` assertion: `GROUP_COUNT * 4 <= u64::BITS` and `GROUP_COUNT * 2 <= u32::BITS`.

**`SemEntry` layout (measured with rustc, x86_64, `Semantics` = 64 B):**

```rust
pub struct SemEntry {
    pub sem: Semantics,              // 64 B; description moves into the existing Arc<SemText>
    pub name: Option<Arc<str>>,      // 16 B
    pub prov: ProvTag,               // u64: 16 groups x 4 bits, ProvCode index
    pub hedge: u32,                  // 16 groups x 2 bits
    pub sem_rev: u32,
    pub known: u16, pub stale: u16, pub fallback: u16,   // GroupMask = u16
}                                    // = 104 B (r2 layout was 120 B; +hedge 128 B)
```

| Layout | Size |
|---|---|
| ADR-0022 r2 (`name` + `description` as `Option<Arc<str>>`, u32 masks) | 120 B |
| r2 + `hedge` | 128 B |
| 20 groups, `ProvTag` u128, `hedge` u64 | 128 B (u128 alignment) |
| **chosen: 16 groups, u16 masks, description in `SemText`** | **104 B** |
| alternative: name + description in one `Arc<NameDesc>` | 96 B (rejected: every name change reallocates the description) |

Gate: `SemEntry ≤ 104 B` (`const` assert); ADR-0022 M-9 (≤ 112 B, 100k-node RSS ≤ 25 MB all
carriers) stays as the ceiling. `ProvCode` (4 bits): `Default, Derived, DerivedTainted, Inferred,
ObservedPrimary, ObservedSecondary, Patch, User, Fallback`, 7 spare.

Primary registers stay inline per node. Secondary, Inferred and evidence registers live in a sparse
side table per writer keyed by (source, scope, anchor), only for nodes that have them, counted against
`max_registers` and M-9.

### 2.13 Profiles (ADR-0016, M14a) and the model: two planes, one trigger matcher

Per-app and per-site variability (owner, 2026-10-19) lives in the profiles system: the typed layer
stack L0..L8 with App/Window/Context/Site triggers (`docs/plan/phase-3-features.md` §3; ADR-0016 to
be written).

| Plane | Input | Output | Owner | Changes when |
|---|---|---|---|---|
| **Fact plane** (this ADR, ADR-0022) | facts, patches, user per-node overrides and rules | `SemEntry` per node | node writer (`FUSE`) | facts arrive |
| **Settings plane** (ADR-0016) | layer files L0..L7, triggers | `Arc<Settings>` per `ProfileKey` | Reader (PM-3) | focus/window/doc changes, edits |

- **PS-1** Profiles never write facts; facts never write settings. Settings gate presentation
  (`ReportFlags`, verbosity, hedge wording) and policy (mode defaults); not fused (ADR-0022 A7).
- **PS-2 Mode defaults are profile keys**: `browse.enabled` (bool), `browse.mode_default`
  (`browse | focus`), `browse.auto_focus_mode` (`widgets | never`), `browse.sticky_focus` (bool),
  all `vendor_profileable`. The Reader's mode machine owns the decision:
  `mode = policy(settings.browse.*, document semantics, user toggle)`. Document semantics (role
  `application`, `aria-busy`, freshness) are model inputs, never settings. The DP-3 sticky-focus
  toggle is session state; "save mode for this app/site" is a PE-1 edit into an App or Site profile.
- **PS-3 Add-ons contribute profile rows as data** (vendor profiles L4, PF-1..PF-4), never hooks. The
  CHR-0 interim per-app mode table uses the PS-2 key names and `AppMatcher` semantics so its rows move
  verbatim into a first-party `ext-chromium` vendor profile (e.g. `code` → `browse.enabled = false`).
  An add-on never activates a profile (PF-3); runtime mode requests stay the M18a `request-mode`
  effect, below the user's toggle.
- **PS-4 Triggers read the model.** `Context` triggers match the focus's published `SemEntry` role
  class; `Site` triggers read `DocMeta.url`. Add-on context tags (PF-3, Phase 4) become evidence facts
  on the document node (`ctx.tag`) that `Context` triggers match.
- **PS-5 Source opt-ins are settings; grants are per scope.** `sources.browser_bus`, `perception.*`
  and per-add-on `contribute-facts` may be set in L3, L5, L6 for App or Site triggers only (so grants do
  not flap with focus), **never by vendor profiles (L4)**. A change bumps that (source, scope) epoch.
- **PS-6 User semantic overrides are not profile keys.** Per-node overrides and "label this control"
  are S2 user facts; user selector rules ("on this site `.icon-x` is Close") are S2 user patches whose
  activation reuses the profiles' `Trigger` type and matcher (`gorca-config`).
- **Terminology.** ADR-0022's `PI[app profile]` and quirk rows are keyed by a **source profile**
  (engine/toolkit class: `gecko`, `blink`, `gtk4`, `electron`), selected by `AppMatcher` from
  first-party or add-on data. It is not a user profile.

### 2.14 What M22a implements and what comes later

| Part | M22a | Later |
|---|---|---|
| Value types (`Fact`, `Subject`, `AnchorKey`, `Confidence`, `Basis`, `BindQuality`, `BindGen`, `SourceId`, frozen `GroupCode`s, `ProvCode`) | **yes** | — |
| `SourceDescriptor`, `SourceGrant`, pure `admit`, pure state-machine `step` (exhaustively tested) | **yes** | — |
| Strata S-1..R including S4, evidence side table, taint, hedge mask, binding generations, epoch snapshot input | **yes** (driven by the fake source writing registers directly) | rule rows for real evidence: M23/M24 |
| Binding for `Node` and `Exact` anchors | **yes** | `DomId`/`Structural` M23, `Region`/`Content` M24 |
| AT-SPI primary source via `gorca-sem-atspi` inside `gorca-doc-source-atspi` and `gorca-connector-atspi` | **yes** | — |
| `FakeFactSource` + conformance kit (pure parts: admission, state machine, revocation equivalence) | **yes** | runtime parts M23-0 |
| `FactPort`, router, source registry runtime, TTL wheel, flood/revocation benches (M-14..M-16) | — | **M23-0** (prelude of M23) |
| Synthetic nodes | key space + flag reserved | M24 |
| AccessKit/Newton primary | — | M19 |
| Browser add-on bus | — | M23 |
| Inference source | — | M24 |
| Annotations | type reserved | M25 |
| WIT `facts`, `prov` | registries only | M18a |
| Hedge wording, refine policy rows | `hedge` stored | M22b1, M24 |

## 3. Alternatives considered

- **A. Numeric weighted fusion across all sources.** Rejected for ranking: different, uncalibrated
  scales; unexplainable; a confident wrong model could override an author's label; no per-group
  trust (math §1.3). Kept in a safe form: thresholds, hedges, class order inside S4, later
  corroboration (D-1).
- **B. Browser add-on as a replacement doc source** (ADR-0001 TBD-S2). Still possible for bulk
  snapshots through `DocSource`, but AT-SPI must stay primary for focus, caret and actions (TBD-S4,
  IR-5). A fact contributor first.
- **C. Inferred values spliced into the name** (Chromium). Rejected: provenance and hedge lost.
- **D. Separate result document for model output** (NVDA). Kept for user-invoked full-window OCR
  (M24); rejected as the only path.
- **E. Facts keyed by node.** Rejected: orphaned on every re-render; anchors + `BindGen` instead.
- **F. One generic transport for primaries too.** Rejected: AT-SPI events already carry the timeline
  seq through a tuned coalescer.
- **G. Secondary wins whenever the primary is `Unknown`** (r1). Rejected (H-3): "not fetched yet" is
  transient and produced phantom state changes.
- **H. Scope declared by the source** (r1). Rejected (H-4): any same-uid process could claim another
  app's documents.
- **I. 128 B entry with 20 groups.** Rejected for 104 B with 16 groups (§2.12): same expressiveness
  (orientation and sort share `flags`' provenance), two fewer cache-line-straddling entries per 4.

## 4. Consequences

- New crates `gorca-fuse` (neutral, tokio-free), `gorca-fact-api` (neutral) and `gorca-sem-atspi`;
  `gorca-model` gains `sem` and `fact`; `SrcNode` gains a normalised `rec: SrcRecord` (H-7).
- Every future source is "implement `FactSource`, pass the conformance kit, get a grant".
- `explain` answers "why did gorca say *probably Delete*": T3 evidence `hint.title_attr`, taint,
  binding `Unique` gen 4, rule row.
- Writers carry an anchor index and side table only when a non-primary source is active.
- ADR-0002's naming cascade becomes S4 data; ADR-0001 TBD-S3 gates M23.
- The M22a runtime is smaller than r1 (M23-0 takes the port and registry).

## 5. Metrics to validate

Δ gates are measured as interleaved A/B, ≥ 3 runs, on a quiet host (`bench/b1/quiet-host.sh`), with
load and governor labelled (M-4).

| Id | Metric | Gate | When |
|---|---|---|---|
| M-9' | `SemEntry` size | ≤ 104 B (`const` assert) | M22a |
| M-14 | Admission cost, fake Secondary at 5k facts/s beside the B-FUSE mix | ≤ 1 µs/fact p99; slices ≤ 50 µs; K2 Δp99 ≤ 0.1 ms | M23-0 |
| M-15 | Flood, one source at 100k facts/s for 60 s | registers and port ≤ caps; `Throttled` ≤ 1 s; other scopes unaffected; **K2 Δp99 ≤ 0.2 ms** | M23-0 |
| M-16 | Revocation of a source holding 100k registers | synchronous part ≤ 0.5 ms and silences focus chain + visible window; full equivalence ≤ 1 s | M23-0 (pure equivalence F14 in M22a) |
| M-17 | Properties F11–F19 | all pass in `cargo test` | M22a |
| M-18 | Binding precision on M8 fixture pages | ≥ 99 % of `Unique` bindings correct; 0 facts through `Heuristic` into semantic groups; 0 rebound facts winning | M23 |
| M-19 | Inferred name precision on the unlabeled-control corpus | ≥ 0.90 `Probable`, ≥ 0.97 `High` | M24 |
| M-20 | Late-fact refine | ≤ 1 refine tail per focus; 0 after an epoch change | M22b1 + M24 |

## 6. Open questions

Owner questions D-1..D-6: kickoff report §6 (final recommendations after review). Engineering
unknowns: cross-source keys for elements without an id (TBD-S3) and native-host pid → browser
mapping under Flatpak/Snap (M23 first task); capture → window mapping and latency through the
gorca-helper `CaptureWindowArea` (ADR-0002 §10, M24); model calibration data (M24); Newton per-tree
ordering (ADR-0022 QO-4); whether CHR-0 changes `SrcNode` (decides the C2c merge order).

## 7. Properties added by this ADR

F11–F17 as in the kickoff report §4, amended: F12 also covers tainted Derived values and requires
that no secondary wins an expressible group; plus **F18** (rebinding: after a `BindGen` bump no fact
asserted under the old generation wins) and **F19** (scope pinning: a fact whose transport scope
differs from its target's scope is rejected and never stored).

## 8. Review traceability (`docs/reports/adr-0024-review.md`)

H-1, H-2 → §2.12. H-3 → §2.2 (S0, class transitions), alt. G. H-4 → §2.3.4, FS-11, F19. H-5 → §2.2
taint, FS-12. H-6 → §2.3.2 `BindGen`, §2.3.3, F18. H-7 → §4 and spec C2. M-1, M-2 → spec §3–§4. M-3
→ §2.14, M23-0. M-4 → §2.6, §2.8, §5. M-5, M-11 → §2.6 port. M-6, M-7 → §2.6 state machine. M-8 →
`SourceId`, §2.10. M-9, L-5 → §2.2 S4, §2.5. M-10 → §2.6 masking. M-12 → TP-3. W-1..W-4 → §2.10.
L-1 → §2.10. L-2 → FS-1. L-3, L-4 → spec.
