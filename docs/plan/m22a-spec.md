# M22a spec: typed semantic model, fact-source contract, one FUSE stage

Status: Proposed (r3, 2026-10-19: x32 layout and oracle findings, `semantic-fusion-math.md` §10; r2 after `docs/reports/adr-0024-review.md`). Owner: architect.
Design: ADR-0022 r2 + r3 note, ADR-0024 r2. Kickoff: `docs/reports/m22a-kickoff.md`. Plan context:
`docs/plan/phase-3.md` §M22.

## 0. Scope

**In:** `gorca-model::sem` and `gorca-model::fact` (x32 layout: 32 group slots, 18 registered, 128 B `SemEntry`); new crates
`gorca-fuse` (pure `FUSE`: registers, evidence side table, strata S-1..S4..R, taint, hedge, binding
generations, `explain`, incremental maintenance), `gorca-fact-api` (contract types, pure `admit`,
pure state-machine `step`), `gorca-sem-atspi` (the one AT-SPI normaliser, AT-SPI quirk rows, default
`PI` and `inexpressible` rows); `FakeFactSource` writing registers directly + pure conformance kit in
`gorca-testkit`; code registries; dual-carry `Arc<SemEntry>` in both writers with legacy agreement
assertions; focus fetch gains concurrent `ATTRS`; `MsgId` + origin on `Segment` (feature); replace X1
and X4; `explain` behind a debug binding.

**Out (moved, review M-3):** `FactPort`, router, source registry runtime, TTL wheel, flood and
revocation-latency benches → **M23-0**. Also out: `gorca-present` and presenter switches (M22b),
real secondary/inferred sources (M23, M24), AccessKit (M19), WIT (M18a), synthetic nodes, hedge
wording (M22b1).

## 1. Crate layout and dependency rules

```
gorca-model           sem/{role,states,entry,codes}.rs, fact.rs; SrcNode.rec   tokio-free, neutral; source of truth for codes
gorca-fuse     (new)  registers, side table, strata, repair, taint, explain,   tokio-free, neutral; deps: gorca-model
                      ivm, quirk engine, trust/inexpressible tables, anchor index (Node/Exact), BindGen
gorca-fact-api (new)  SourceDescriptor, SourceGrant, StableIdentity, admit(),  neutral; gorca-model (runtime types
                      SourceState + step(), SourceEpoch                         and gorca-rt only from M23-0)
gorca-sem-atspi (new) raw (role u32, states u64, attrs, relations) ->          tokio-free, neutral; no zbus, no atspi crate
                      SrcRecord; quirks/*.toml, pi/*.toml, inexpressible/*.toml
gorca-testkit         fake_source.rs, fact_conformance.rs                       test-only
gorca-ext-api         codes.rs: generated copy of the gorca-model registries + equality test (L-1)
```

`xtask/src/deps.rs`: new crates in `NEUTRAL`; `gorca-fuse`, `gorca-sem-atspi` in `TOKIO_FREE`; only
`gorca-connector-atspi` and `gorca-doc-source-atspi` (and tests) may depend on `gorca-sem-atspi`
(**the normaliser runs in those crates, never in `gorca-core`**, H-7); `gorca-core` never depends on
`gorca-source-*` (L-2).

## 2. Coordination with CHR-0 (running in parallel on doc-source-atspi/browse)

CHR-0 owns until it merges: `gorca-doc-source-atspi/src/source.rs`, `gorca-core/src/doc_registry.rs`,
`app_actor/doc_route.rs`, `doc_actor/mod.rs`, `browse/latency.rs`, `gorca-connector-atspi/src/toolkit.rs`,
`browse/commands.rs` (loading grace, `web_changed`, re-join), `gorca-dbus/src/drain/map.rs`, and its
interim per-app mode table (shaped per ADR-0024 §2.13 PS-2/PS-3). **Waves A and B touch none of
these.** Wave C starts only after CHR-0 and P2-ARIA-0 have merged and rebases on them. Whether CHR-0
changes `SrcNode` (`gorca-model/src/doc.rs`) decides the C2c merge order; A1 does **not** touch
`doc.rs` (the `rec` field is added in C2c).

## 3. Task breakdown (≤ 2 days each)

| Id | Task | Owner | Days | Depends on | Files (owned) |
|---|---|---|---|---|---|
| **A1** | Model value types per ADR-0024 §2.12 **layout x32**: `Semantics` (≤ 80 B, 64 B expected; `description` and `roledescription` in `Arc<SemText>`), `SemRole` (u16 codes), tri-state enums, `GroupCode` with `GROUP_CAP = 32` and the **18 registered groups** in code order (evidence codes bit 15), `SemEntry` (x32: `ProvTag([u64;2])`, `hedge: u64`, `taint: u64`, u32 masks; `const` asserts per §2.12), `ProvCode` without `Fallback`, `fact.rs` (`Fact`, `Subject`, `AnchorKey` all cases, `Confidence`, `ConfClass`, `Basis` incl. `Snapshot`, `BindQuality`, `BindGen`, `SourceId{slot:u32,gen:u32}`, `SourceClass`/`TrustTier` as u8 codes); registries in `gorca-model`, generated copy + equality test in `gorca-ext-api`; empty crates and deps rules | rust-engineer (R1) | 1.5 | — | `gorca-model/src/{sem/,fact.rs,lib.rs}`, `gorca-ext-api/src/codes.rs`, `crates/gorca-{fuse,fact-api,sem-atspi}/` stubs, root `Cargo.toml` (members), `xtask/src/deps.rs` |
| **A2** | Raw AT-SPI semantic fixture corpus (Firefox, Chromium, GTK4), private sessions; JSONL + `expect.toml` for ≈ 60 APG patterns, engine disagreements marked | linux-platform-engineer (L1); web-a11y-engineer reviews | 2 | — | `tests/fixtures/sem/**`, `tools/sem-capture/` |
| **A3** | Reference oracle: extend `semantic_fusion.py` with classes, S4, evidence, taint, floors/classes/hedge, `vis_rev` and `BindGen` staleness, binding minimums, static `inexpressible` rows, TTL as lifecycle, epochs as inputs; JSON trace corpus; check F11–F19 | math-researcher (M1) | 2 | — | `tools/research/semantic_fusion.py`, `tests/fixtures/fuse/**` |
| A4 | A-vs-C representation micro-bench (ADR-0022 §3) on the x32 layout (math §10.1 already measured copy costs; A4 adds compose cost) | perf-engineer (P1) | 1.5 | A1 | `crates/gorca-bench/benches/sem_repr.rs` |
| B1a | `gorca-fuse` core: inline primary registers, strata S-1, S0 (with static `inexpressible` candidacy), S1, S2, S3, R, fill rules, `ProvTag`; from scratch; name from content with untainted children first; differential tests vs `tests/fixtures/fuse/fuse-x32.json` (target) and `fuse-r2-16.json` (same semantics, older layout; must agree on values, provenance, hedge and taint); F1–F6, F9, F10 | R1 | 2 | A1, A3 | `gorca-fuse/src/{register,strata,repair}.rs` |
| B1b | `gorca-fuse` sources: evidence side table keyed (source, scope, anchor), S4 ranked by the tier-capped `ConfClass` then `(kind, source id, direct before rule, value, anchor, seq)`, taint propagation and `taint` mask, hedge mask, `BindGen` stamped at admission and carried in the register (stale when ≠ current), epoch snapshot as input, conflict diagnostics, `explain` (trace mode); F11–F13, F15, F18 | R1 | 2 | B1a | `gorca-fuse/src/{side,s4,taint,explain}.rs` |
| B2 | `gorca-fuse` incremental: dirty set, early cutoff, reverse indices (content-name, relation cap 64, selector-read stub), `vis_rev`, batch budget with focus-chain priority, global and per-(source, scope) epochs with synchronous focus-chain/visible-window re-fuse hook; `fuse_full_check`; F7, F14 | R1 | 2 | B1b | `gorca-fuse/src/{ivm,epoch}.rs` |
| B3 | `gorca-fact-api` pure contract: descriptor, grant, `StableIdentity`, `admit()` (all ADR-0024 §2.6 checks incl. FS-11 scope pinning, truncation of oversize page strings), `SourceState` + `step()` per (source, scope) with the r2 table (exhaustive `match`, stable-identity memory), `SourceEpoch`; `FakeFactSource` (any class; scripted values, confidences, bases, generations, malformed and out-of-scope facts, detach/revoke) and the pure conformance kit; F16 (pure part), F19 | rust-engineer (R2) | 2 | A1 | `crates/gorca-fact-api/**`, `gorca-testkit/src/{fake_source,fact_conformance}.rs` |
| B4 | `gorca-sem-atspi` normaliser: role table (`xml-roles` tokens, multi-token fallback, invalid tokens, engine role vs ARIA rule), one state mapping (INDETERMINATE → `Check::Mixed` for checkables), attributes (level, posinset, setsize, placeholder-text, keyshortcuts, valuetext, live/relevant/atomic/busy, current, haspopup, invalid, roledescription, colindextext/rowindextext, tag), `Unknown` vs `Absent` from the fetched mask; tests on the A2 corpus; M-10 unmapped list | R2 | 2 | B3, A2 | `crates/gorca-sem-atspi/src/{roles,states,attrs,record}.rs` |
| B5 | Quirk engine + tables: closed `QuirkAction` and loader in `gorca-fuse`; AT-SPI rows in `gorca-sem-atspi/quirks/` (Gecko list-marker row from X1, Blink rows after verifying `xml-roles`), default `PI` and empty `inexpressible` rows; source-profile selection by `AppMatcher` data; hit ids in `explain` | R2 | 1.5 | B1a, B4 | `gorca-fuse/src/quirk.rs`, `gorca-sem-atspi/{quirks,pi,inexpressible}/` |
| B6 | B-FUSE bench: 10k–50k mixed events/s, 100k-node mirror, 100 patches, a fake secondary writing registers at 5k facts/s; M-11 | P1 | 1.5 | B2 | `crates/gorca-bench/benches/fuse.rs`, `bench/m22a/` |
| C1a | Focus fetch issues `ATTRS` (and web relations) concurrently in the same batch; review/object-nav masks gain `ATTRS`; **M-6b cold-focus measurement first** (Firefox, Chromium with large `class` attrs, GTK4); lazy-`ATTRS` fallback switch if the gate fails | R1 + P1 | 1.5 | CHR-0 merged | `gorca-core/src/present/flow.rs` (`FOCUS_FIELDS` only), `review/objnav.rs` (mask only), `gorca-connector-atspi/src/` fetch |
| C1b | App actor writer: registers + `FUSE` per app, `NodeSnapshot` carries `Arc<SemEntry>`; `gorca-connector-atspi/src/decode.rs` calls `gorca-sem-atspi`; legacy agreement `debug_assert` + CI counter; `Segment` gains `MsgId` + origin behind `feature = "seg-origin"` | R1 | 2 | B2, B5, C1a | `gorca-core/src/app_actor*.rs`, `gorca-connector-atspi/src/decode.rs`, `gorca-model/src/speech.rs` |
| C2 | Doc writer (H-7): `gorca-doc-source-atspi` runs `gorca-sem-atspi` and fills a new `SrcNode.rec: SrcRecord` (`gorca-model/src/doc.rs`, after CHR-0); doc actor holds `Arc<SemEntry>` in `NodeMeta`, fuses into the left-right back copy before `publish()`; focus-fetch facts of web nodes forwarded to the doc actor as `atspi` facts (SM-8); `is_gecko`/`is_blink` branches → quirk rows; `translate.rs` `web_attrs` parsing (TAG, XML_ROLES, LIVE, LEVEL) moves into the normaliser | R2 | 2 | B2, B5, CHR-0 merged | `gorca-model/src/doc.rs`, `gorca-doc-source-atspi/src/{factory,roles}.rs`, `gorca-core/src/doc_actor/{translate,live}.rs`, `gorca-doc/src/web/` |
| C3 | Replace X1 and X4: S3 derived rules replace `subtree_label` and the list-marker branch; delete `web_overlay` merging; delete the duplicate `map_states`; P2-ARIA-0 tests stay green | R1 | 2 | C1b, C2, P2-ARIA-0 merged | `browse/commands.rs` (`web_overlay`, `subtree_label` only), `present/flow.rs` (`spoken_snapshot` call site only) |
| C4 | `explain` diagnostics: debug binding "explain last utterance" (dev builds), segment-origin log line; AT-SPI registered as Primary `SourceId{0,0}` in the daemon | R2 | 1 | C1b, C2 | `gorca-daemon/src/wire_sources.rs`, `gorca-core/src/commands/diag.rs` |
| C5 | Gates: F1–F19 + applicable P0–P10 (**F17 tested here only, against the real pipeline**: refine tails ≤ 1, same first utterance with and without in-flight non-primary facts); legacy agreement = 0 on M8 e2e (Firefox, Chromium) + GTK4 fixture; K2/X1 regression runs (interleaved A/B, ≥ 3 runs, quiet host) | test-engineer (T1) + P1 | 1.5 | C3, C4 | `tests/`, `bench/e2e/` rows |
| C6 | Adversarial review, fix round, decision row | reviewer, then R1/R2 | 2–3 | C5 | — |

**Critical path** A1 → B1a → B1b → B2 → C1b → C3 → C5 → C6 ≈ **18–20 working days** with two Rust
engineers and 2–3 review rounds (review M-2). B3/B4/B5 run beside B1/B2; C1a runs as soon as CHR-0
merges, even before wave B ends.

## 4. Parallelism and host limits

- At most **4 implementers** and at most **3 concurrent cargo builders** (one build each,
  `CARGO_BUILD_JOBS=6`); check `free -g` and `/proc/pressure/memory` before B6 and C5.
- **Wave A:** R1 (A1, builds), L1 (A2, Python + private browsers, no cargo), M1 (A3, Python), P1 (A4
  from day 2, builds) → 2 builders. **One builder slot is free in wave A**: it goes to M14a-core only
  if ADR-0016 is written and accepted by then (D-6); otherwise it stays empty.
- **Wave B:** R1 (B1a → B1b → B2), R2 (B3 → B4 → B5), P1 (B6 after B2; idle or A4 follow-ups before)
  → 3 builders. L1 reviews B4 against the A2 corpus (no build). No M14a slot in wave B.
- **Wave C:** R1 (C1a → C1b → C3), R2 (C2 → C4), T1 + P1 (C5) → 3 builders.
- **M23-0** (port, router, registry runtime, TTL wheel, M-14..M-16 benches) is scheduled with M23,
  not here.

## 5. Test strategy

1. **Differential oracle.** `gorca-fuse` reproduces every A3 trace (entries, `ProvTag`, hedge, taint,
   trails) over canonical, reversed, shuffled-with-duplicate deliveries and random batch cuts.
2. **Property tests** (`proptest`, tier 1): F1–F10 (math §5), F11–F19 (ADR-0024 §7, kickoff §4), trees
   ≤ 400 nodes, fake Primary/Secondary/Inferred + patches + user overrides.
3. **Pure conformance kit**: seq monotonicity, every admission rejection reason, out-of-scope facts
   (F19), oversize strings truncated, state machine exhaustively (every state × event, stable identity
   across reconnect), revocation equivalence (F14), no fact about a secret node accepted.
4. **Normaliser golden tests** on the A2 corpus and M-10 coverage.
5. **Legacy agreement (dual-carry)** in debug/CI builds, by (node, group).
6. **Regression e2e:** m7/m8 (Firefox, Chromium), m3/m4 focus, X1 extension suite; flakes labelled and
   rerun, never overwritten.

## 6. Acceptance numbers (merge gate for M22a)

| Gate | Number |
|---|---|
| Size | `Semantics` ≤ 80 B, `SemEntry` ≤ 160 B (`const` asserts; x32 measures 128 B); 100k-node RSS increase ≤ 25 MB, all carriers (the real ceiling) |
| M-10 coverage | every ARIA 1.2 role/state mapped or listed |
| M-11 B-FUSE | fusion + publish p99 ≤ 2 µs/event at 10k–50k events/s; no ingress lag regression |
| M-12/M-17 | F1–F19 and applicable P0–P10 pass |
| M-6b | cold focus keypress → speech Δp99 ≤ 2 ms (Firefox, Chromium, GTK4), else lazy `ATTRS` |
| K2 | key → speech gorca-side Δp99 ≤ 0.1 ms, interleaved A/B, ≥ 3 runs, quiet host |
| Agreement | 0 legacy disagreements on M8 + GTK4 fixtures outside `deviations.toml` |
| Rust gates | `cargo fmt`, `cargo clippy -- -D warnings`, `cargo test --workspace`, `cargo xtask deps` |

## 7. First tasks, ready to start (self-contained specs)

### A1 — Model value types (rust-engineer)

Read `CLAUDE.md`, ADR-0022 §2.1–§2.6 (and its r3 note), ADR-0024 §2.1–§2.6, §2.10, §2.12, and
`docs/reports/semantic-fusion-math.md` §10.1 (layout study, `research/semsize/layout.rs`). Run
`. ./env.sh`. Create `crates/gorca-model/src/sem/` and `crates/gorca-model/src/fact.rs` with the types
named there (sketch in `docs/reports/aria-semantics-architecture.md` §3.1, **without** a provenance
field). Rules:

- **Layout x32** exactly as ADR-0024 §2.12: `GROUP_CAP = 32`; 18 registered `GroupCode`s in this code
  order: `role, name, description, check, press, expand, select, current, invalid, popup, avail,
  flags, level, setpos, grid, live, orientation, sort` (append-only; evidence codes have bit 15 set);
  `GroupMask = u32` for `known`/`stale`/`fallback`; `ProvTag([u64; 2])` (not `u128`); `hedge: u64`
  and `taint: u64` (2 bits per slot); `sem_rev: u32`; `name: Option<Arc<str>>`; `description` and
  `roledescription` in `Arc<SemText>`.
- `const` asserts: `size_of::<Semantics>() <= 80`, `size_of::<SemEntry>() <= 160`,
  `GROUP_COUNT <= GROUP_CAP`, `GROUP_CAP * 4 <= 128`, `GROUP_CAP * 2 <= 64`. Expected today: 64 B and
  128 B; report the measured numbers.
- `ProvCode` (u8 code, 4 bits in `ProvTag`): `Default, Derived, DerivedTainted, Inferred,
  ObservedPrimary, ObservedSecondary, Patch, User` (**no `Fallback`**: fallback is the mask bit).
- `#[repr(u16)]` append-only `SemRole`; tri-state enums instead of bit pairs; `Unknown` is the `known`
  mask, not an enum variant; `SourceClass`, `TrustTier`, `ConfClass` as `u8` codes;
  `SourceId { slot: u32, gen: u32 }`; `BindGen(u32)`; every `AnchorKey` case and `Basis::Snapshot`.
- Codes must match the oracle tables in `tests/fixtures/fuse/fuse-x32.json` (target layout; the
  `fuse-r2-16.json` corpus stays as a second fixture for B1): add a test that parses the header's
  group/ProvCode/ConfClass/BindQuality tables and compares them with the Rust registries.
- Registries are defined in `gorca-model`; `crates/gorca-ext-api/src/codes.rs` holds a generated
  name ↔ code copy with an equality test and a checked-in append-only snapshot.
- Add `gorca-fuse`, `gorca-fact-api`, `gorca-sem-atspi` as stub crates (root `Cargo.toml` members) and
  the `xtask/src/deps.rs` rules of §1. Do not touch `gorca-model/src/doc.rs` or any other crate.

Done = Rust gates green; measured sizes in the PR description.

### A2 — Raw AT-SPI semantic fixture corpus (linux-platform-engineer)

In **private sessions only** (own dbus-broker and a11y bus, unique Wayland display and runtime dir,
kill only recorded PIDs; never the live session), load the M8 fixture pages (`tests/web/pages/`, APG
patterns) in Firefox ESR and Chromium (`--force-renderer-accessibility`) and a GTK4 widget gallery
(`/usr/bin/python3` + PyGObject). Record per accessible: raw role (u32 and name), state bits,
`GetAttributes`, relations, interfaces, name, description, child count, fixture element id. Write
`tests/fixtures/sem/{firefox,chromium,gtk4}/<page>.jsonl` with a header (versions, date) and the
capture tool in `tools/sem-capture/`. Draft `tests/fixtures/sem/expect.toml` for ≈ 60 APG patterns
(expected normalised role, tri-states, level, position, name source), marking engine disagreements and
attributes one engine never exports (input to the `inexpressible` rows). Web-a11y-engineer reviews
the expectations. No crate edits.

### A3 — Reference oracle for sources (math-researcher)

Extend `tools/research/semantic_fusion.py` (existing results stay reproducible) with ADR-0024 r2:
classes Primary/Secondary/Inferred; S0 secondary candidacy only through a static `inexpressible`
table; S4 below S3 with the fill rule and `(ConfClass, kind, id)` order; evidence read only by S3/S4
(toy rules: `title` → name, `visual.ocr_text` → name); taint (lowest tier and class of inputs);
floors, class thresholds, hedge; staleness by `vis_rev` and `BindGen`; binding minimums; TTL as a
lifecycle event; epochs as explicit inputs. Check F11–F19 on random workloads as in math §6. Emit a
deterministic JSON corpus to `tests/fixtures/fuse/` (schema in the file header): facts, delivery
orders, expected entries, `ProvTag` codes, hedge and taint masks, `explain` trails. Results in a new
§10 of `docs/reports/semantic-fusion-math.md`. Python only.
