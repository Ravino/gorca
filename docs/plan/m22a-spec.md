# M22a spec: typed semantic model, fact-source contract, one FUSE stage

Status: Proposed (2026-10-19). Owner: architect. Design: ADR-0022 r2 (+ r3 note), ADR-0024.
Kickoff analysis: `docs/reports/m22a-kickoff.md`. Plan context: `docs/plan/phase-3.md` §M22.

## 0. Scope

**In:** `gorca-model::sem` and `gorca-model::fact`; new crates `gorca-fuse` (pure FUSE, registers,
strata S-1..S4..R, `explain`, incremental maintenance), `gorca-fact-api` (descriptor, grant,
admission, `FactPort`, source state machine, epochs), `gorca-sem-atspi` (the one AT-SPI normaliser,
AT-SPI quirk rows, default `PI` rows); `FakeFactSource` + conformance kit in `gorca-testkit`; code
registries in `gorca-ext-api`; dual-carry `Arc<SemEntry>` in both writers (app actor, doc actor)
with legacy agreement assertions; focus fetch gains concurrent `ATTRS`; `MsgId` + origin on
`Segment` (test/diagnostic feature); replace X1 (`web_overlay`, `subtree_label`) and X4 (second
normaliser); `explain` behind a debug binding.

**Out:** `gorca-present` and any presenter switch (M22b1..b4); real secondary/inferred sources (M23,
M24); AccessKit (M19); WIT (M18a); synthetic nodes (reserved only); hedge wording (M22b1).

## 1. Crate layout and dependency rules

```
gorca-model          sem/{role.rs, states.rs, entry.rs, codes.rs}, fact.rs      tokio-free, neutral
gorca-fuse   (new)   registers, side table, strata, repair, explain, ivm,      tokio-free, neutral;
                     quirk engine, trust table, anchor index (Node/Exact)      deps: gorca-model only
gorca-fact-api (new) SourceDescriptor, SourceGrant, admission, FactPort,       neutral; gorca-model,
                     SourceState machine, SourceEpoch, registry trait          gorca-rt
gorca-sem-atspi (new) raw (role u32, states u64, attrs, relations) -> SrcRecord; neutral, no zbus,
                     quirks/*.toml, pi/*.toml                                  no atspi crate
gorca-testkit        fake_source.rs, fact_conformance.rs                        test-only
gorca-ext-api        codes.rs (SemRole, GroupCode, Field registries as data)
```

`xtask/src/deps.rs`: add the three new crates to `NEUTRAL`; `gorca-fuse` and `gorca-sem-atspi` to
`TOKIO_FREE`; a new rule: `gorca-core` may depend on `gorca-fuse`, `gorca-fact-api`; only
`gorca-connector-atspi` and `gorca-doc-source-atspi` (and tests) depend on `gorca-sem-atspi`.

## 2. Coordination with CHR-0 (running in parallel on doc-source-atspi/browse)

CHR-0 owns until it merges: `gorca-doc-source-atspi/src/source.rs`, `gorca-core/src/doc_registry.rs`,
`app_actor/doc_route.rs`, `gorca-connector-atspi/src/toolkit.rs`, `browse/commands.rs` (loading
grace, `web_changed`, re-join), `gorca-dbus/src/drain/map.rs`, and its interim per-app mode table.
**M22a waves A and B touch none of these.** Wave C (writers, X1/X4 replacement, `factory.rs`
`is_gecko`/`is_blink` removal) starts only after CHR-0 and P2-ARIA-0 have merged, and rebases on
them. CHR-0's interim mode table follows ADR-0024 §2.13 PS-2/PS-3 key names so M14a absorbs it.

## 3. Task breakdown (≤ 2 days each)

| Id | Task | Owner | Days | Depends on | Files (owned) |
|---|---|---|---|---|---|
| **A1** | Model value types: `Semantics` (≤ 64 B), `SemRole` (u16 codes, ARIA 1.2 + DPUB subset + native + r2 additions), tri-state enums, `SemEntry` (+`hedge`), `GroupMask`, `ProvTag`/`ProvCode`, `fact.rs` (`Fact`, `Subject`, `AnchorKey`, `Confidence`, `ConfClass`, `Basis`, `BindQuality`, `SourceId`, `SourceClass`, `TrustTier`, `GroupCode` semantic + evidence ranges); code registries as data in `gorca-ext-api`; size tests | rust-engineer (R1) | 1.5 | — | `gorca-model/src/sem/`, `gorca-model/src/fact.rs`, `gorca-model/src/lib.rs` (re-exports), `gorca-ext-api/src/codes.rs`, `xtask/src/deps.rs` |
| **A2** | Raw AT-SPI semantic fixture corpus: capture `(role, states, attributes, relations, interfaces, name, description)` for the M8 fixture pages (Firefox, Chromium) and a GTK4 widget gallery, private sessions only; JSONL + a hand-checked expectation file for ≈ 60 APG patterns | linux-platform-engineer (L1), web-a11y-engineer reviews expectations | 2 | — | `tests/fixtures/sem/**`, `tools/sem-capture/` |
| **A3** | Reference oracle: extend `tools/research/semantic_fusion.py` with source classes, S4, evidence, confidence floors, basis/`vis_rev` staleness, binding quality, TTL, source epochs/revocation; emit a JSON trace corpus (facts in several delivery orders → expected `SemEntry`, `ProvTag`, hedge, `explain` trail); check F11–F17 on the model | math-researcher (M1) | 2 | — | `tools/research/semantic_fusion.py`, `tests/fixtures/fuse/**` |
| A4 | A-vs-C representation micro-bench (ADR-0022 §3): compose cost and memory on a 100k-node synthetic page; reviewer judges on M-4/M-5/M-9 | perf-engineer (P1) | 1.5 | A1 | `crates/gorca-bench/benches/sem_repr.rs` |
| B1 | `gorca-fuse` from scratch: inline primary registers, sparse side table (source, anchor), strata S-1..S0..S1..S2..S3..S4..R, fill rules (A6), hedge mask, `ProvTag`, conflict diagnostics, `explain` (trace mode, generic tracer); differential tests against the A3 corpus; F1–F6, F9–F13 | R1 | 2 | A1, A3 | `crates/gorca-fuse/src/{register,strata,repair,explain}.rs` |
| B2 | `gorca-fuse` incremental: dirty set, early cutoff, reverse indices (content-name edges, relation reverse index cap 64, selector-read index stub for M18a), `vis_rev`, TTL timer wheel, batch budget with focus-chain priority, global epochs (patch set, quirk, `PI`, per-source); `fuse_full_check` test feature; F7, F14 | R1 | 2 | B1 | `crates/gorca-fuse/src/{ivm,epoch,ttl}.rs` |
| B3 | `gorca-fact-api`: descriptor, grant, admission (pure), `FactPort` (coalescing by (subject, group), token bucket, `offer` results), `SourceState` machine with the ADR-0024 §2.6 table (exhaustive `match`), `SourceEpoch`, registry trait; `FakeFactSource` (any class; scripted values, confidences, bases, floods, malformed facts, detach, revoke) and `fact_conformance` kit in testkit; F15, F16 | rust-engineer (R2) or test-engineer | 2 | A1 | `crates/gorca-fact-api/**`, `gorca-testkit/src/{fake_source,fact_conformance}.rs` |
| B4 | `gorca-sem-atspi` normaliser: one role table (incl. `xml-roles` tokens, multi-token fallback, invalid tokens, engine-computed vs ARIA role rule), one state mapping (INDETERMINATE → `Check::Mixed` for checkables, audit G1), attribute parsing (level, posinset, setsize, placeholder-text, keyshortcuts, valuetext, live/relevant/atomic/busy, current, haspopup, invalid, roledescription, colindextext/rowindextext), `Unknown` vs `Absent` from the fetched mask; tests on the A2 corpus; M-10 unmapped list | L1 (or R2) | 2 | A1, A2 | `crates/gorca-sem-atspi/src/{roles,states,attrs,record}.rs` |
| B5 | Quirk engine + trust table: closed `QuirkAction` enum and table loader in `gorca-fuse`; AT-SPI rows in `gorca-sem-atspi/quirks/` (Gecko list-marker name row from X1, Blink rows after verifying `xml-roles`), default `PI` rows (primary before secondary); source-profile selection by `AppMatcher` data; hit ids into `explain` | R2 | 1.5 | B1, B4 | `gorca-fuse/src/quirk.rs`, `gorca-sem-atspi/{quirks,pi}/` |
| B6 | B-FUSE bench: 10k–50k mixed events/s, 100k-node mirror, 100 patches; plus fake secondary at 5k facts/s, flood 100k facts/s, revocation of 100k registers; M-11, M-14, M-15, M-16 | P1 | 2 | B2, B3 | `crates/gorca-bench/benches/fuse.rs`, `bench/m22a/` |
| C1 | App actor writer: `NodeSnapshot` carries `Arc<SemEntry>`; registers + FUSE per app; focus fetch issues `ATTRS` concurrently in the same batch (and web relations); review/object-nav masks gain `ATTRS`; legacy agreement `debug_assert` + CI counter; `Segment` gains `MsgId` + origin behind `feature = "seg-origin"` | R1 | 2 | B2, B4, CHR-0 merged | `gorca-core/src/app_actor*.rs`, `gorca-connector-atspi/src/decode.rs` (call `gorca-sem-atspi`), `gorca-model/src/speech.rs` |
| C2 | Doc actor writer: `SrcNode` → `SrcRecord` via `gorca-sem-atspi`; `NodeMeta` holds `Arc<SemEntry>`; focus-fetch facts of web nodes forwarded to the doc actor as `atspi` facts (SM-8); `is_gecko`/`is_blink` branches → quirk rows; FUSE results into the left-right back copy before `publish()` | R2 | 2 | B2, B5, CHR-0 merged | `gorca-core/src/doc_actor/`, `gorca-doc/src/web/`, `gorca-doc-source-atspi/src/{factory,roles}.rs` |
| C3 | Replace X1 and X4: S3 derived rules replace `subtree_label` and the list-marker branch; delete `web_overlay` merging (presenter reads the fused entry); delete `map_states` duplicate; P2-ARIA-0 tests stay green | R1 | 2 | C1, C2, P2-ARIA-0 merged | `browse/commands.rs` (`web_overlay`, `subtree_label` only), `present/flow.rs` (`spoken_snapshot` call site only) |
| C4 | `explain` diagnostics: debug binding "explain last utterance" (dev builds) and a log line per utterance segment origin; source registry wiring in the daemon (`wire_sources`), AT-SPI registered as Primary `SourceId(0)` | R2 | 1 | C1, C2 | `gorca-daemon/src/wire_sources.rs`, `gorca-core/src/commands/diag.rs` |
| C5 | Gates: F1–F17 + applicable P0–P10 in `cargo test`; legacy agreement = 0 on M8 e2e fixtures (Firefox, Chromium) and the GTK4 fixture; M-6b cold-focus measurement (Firefox + GTK4); K2/X1 regression runs | test-engineer (T1) + P1 | 1.5 | C3, C4 | `tests/`, `bench/e2e/` scenario rows |
| C6 | Adversarial review, fix round, decision row | reviewer, then R1/R2 | 1–2 | C5 | — |

Critical path: A1 → B1 → B2 → C1 → C3 → C5 → C6 ≈ 12–13 working days with two Rust engineers;
A2/A3 overlap with A1; B3/B4/B5 overlap with B1/B2.

## 4. Parallelism and host limits

- At most **4 implementers** at once and at most **3 cargo-building** agents (one build each,
  `CARGO_BUILD_JOBS=6`); check `free -g` and `/proc/pressure/memory` before B6 and C5.
- Wave A: R1 (A1, builds), L1 (A2: private browsers + GTK app, Python only, memory-heavy but no
  cargo), M1 (A3, Python only). A4 joins on day 2 (P1, builds).
- Wave B: R1 (B1 → B2), R2 (B3 → B5), L1 (B4), P1 (B6 after B2/B3). Disjoint crates.
- Wave C: R1 (C1 → C3), R2 (C2 → C4), T1 + P1 (C5). C1/C2 touch different actors; C3 is serial
  after both.
- A fourth slot is free during waves A and B for **M14a-core** (see `docs/reports/m22a-kickoff.md`
  §5): `gorca-config` + Reader settings only, disjoint from every M22a file.

## 5. Test strategy

1. **Differential oracle.** The A3 Python model emits trace corpora; `gorca-fuse` must reproduce
   every expected entry, provenance and trail bit-for-bit (canonical, reversed, shuffled-with-dup
   deliveries, random batch cuts).
2. **Property tests** (`proptest`, tier 1 in `cargo test`): F1–F10 (math report §5) and F11–F17
   (kickoff report §4) over generated trees ≤ 400 nodes, 3 fake sources (Primary, Secondary,
   Inferred) + patches + user overrides.
3. **Conformance kit** every source must pass (fake now, AT-SPI adapter in C1/C2, later M19/M23/M24):
   seq monotonicity, admission rejections for each reason, flood → `Throttled`, detach/re-attach,
   revocation equivalence, no fact about a secret node accepted.
4. **Normaliser golden tests** on the A2 corpus (raw → `SrcRecord`), and M-10 coverage (every ARIA
   1.2 role/state mapped or listed unmapped).
5. **Legacy agreement (dual-carry).** In debug/CI builds every legacy read (`AttrMap`, `StateSet`,
   `web_attrs`) is compared with the entry; disagreements fail tests and are listed by (node, group).
6. **Regression e2e:** m7/m8 (Firefox, Chromium), m3/m4 focus suites, X1 extension suite; no new
   failures; flakes labelled and rerun, never overwritten.
7. **Model checking of the source state machine:** exhaustive transition test (every state × event)
   against the ADR-0024 table.

## 6. Acceptance numbers (merge gate for M22a)

| Gate | Number |
|---|---|
| M-9 memory | `Semantics` ≤ 64 B; `SemEntry` ≤ 112 B; 100k-node page RSS increase ≤ 25 MB, all carriers |
| M-10 coverage | every ARIA 1.2 role/state mapped or listed |
| M-11 B-FUSE | fusion + publish p99 ≤ 2 µs/event at 10k–50k events/s; no ingress lag regression |
| M-12/M-17 | F1–F17 and applicable P0–P10 pass |
| M-14 | admission ≤ 1 µs/fact p99 at 5k facts/s; K2 key → speech Δp99 ≤ 0.1 ms |
| M-15 | 100k facts/s flood: registers ≤ cap, `Throttled` ≤ 1 s, primary path unchanged |
| M-16 | revocation turn ≤ 50 µs; equivalence within 1 s |
| M-6b | cold focus keypress → speech Δp99 ≤ 2 ms (Firefox + GTK4), else lazy `ATTRS` fallback |
| Agreement | 0 legacy disagreements on M8 + GTK4 fixtures (or each listed with a reason in `deviations.toml`) |
| Rust gates | `cargo fmt`, `cargo clippy -- -D warnings`, `cargo test --workspace`, `cargo xtask deps` |

## 7. First tasks, ready to start (self-contained specs)

### A1 — Model value types (rust-engineer)

Read `CLAUDE.md`, ADR-0022 §2.1–§2.6, ADR-0024 §2.1–§2.6 and §2.12. Run `. ./env.sh`. Create
`crates/gorca-model/src/sem/` and `crates/gorca-model/src/fact.rs` with exactly the types named in
those sections (sketch in `docs/reports/aria-semantics-architecture.md` §3.1, **without** a
provenance field). Rules: `#[repr(u16)]` codes for `SemRole` and `GroupCode`, append-only; tri-state
enums instead of bit pairs; `Unknown` is not a variant of value enums (it is the `known` mask);
`Confidence { class: ConfClass, p_milli: u16 }`; `Subject`, `AnchorKey`, `Basis`, `BindQuality` as
in ADR-0024 §2.3–§2.4. Add `const` size assertions (`Semantics` ≤ 64 B, `SemEntry` ≤ 112 B; if
`SemEntry` exceeds it, pack the masks into one `u128` per ADR-0024 §2.12). Emit the registries as
data in `crates/gorca-ext-api/src/codes.rs` (name ↔ code tables, test that codes are unique and
append-only against a checked-in snapshot). Add `gorca-fuse`, `gorca-fact-api`, `gorca-sem-atspi` as
empty crates with `lib.rs` stubs and register them in `xtask/src/deps.rs` (§1). Do not touch any
other crate. Done = Rust gates green, size test numbers in the PR description.

### A2 — Raw AT-SPI semantic fixture corpus (linux-platform-engineer)

Goal: real inputs for the normaliser and the agreement gate. In **private sessions only** (own
dbus-broker/a11y bus, unique Wayland display, kill only recorded PIDs; never the live session), load
the M8 fixture pages (`tests/web/pages/`, APG patterns) in Firefox ESR and Chromium
(`--force-renderer-accessibility`) and a GTK4 widget gallery (`/usr/bin/python3` + PyGObject). For
every accessible record: role (raw u32 and name), state bit set, `GetAttributes`, relation set,
interfaces, name, description, child count, and the fixture element id. Write JSONL to
`tests/fixtures/sem/{firefox,chromium,gtk4}/<page>.jsonl` with a header (browser version,
at-spi2-core version, date) and a capture tool in `tools/sem-capture/` (Python, project venv for
analysis; system python for GTK apps). Then draft `tests/fixtures/sem/expect.toml`: for ≈ 60 APG
patterns, the expected normalised role, tri-states, level, position, name source; mark every case
where engines disagree. Web-a11y-engineer reviews the expectations. No crate edits.

### A3 — Reference oracle for sources (math-researcher)

Extend `tools/research/semantic_fusion.py` (keep existing results reproducible) with ADR-0024:
source classes Primary/Secondary/Inferred, stratum S4 below S3 with the fill rule, evidence facts
read only by S3/S4 rules (one toy rule each: `title` → name; `visual.ocr_text` → name), confidence
floors and `ConfClass` thresholds (hedge output), `Basis::VisRev` staleness, `BindQuality` minimums
per group class, TTL expiry as a lifecycle event, per-source epochs (mask/revoke). Prove or check
F11–F17 (kickoff report §4) on random workloads as in §6 of the math report. Emit a deterministic
JSON corpus to `tests/fixtures/fuse/` (schema documented in the file header): facts, delivery
orders, expected effective entries, `ProvTag` codes, hedge masks and `explain` trails. Report
numbers in `docs/reports/semantic-fusion-math.md` §10 (new section). Python only.
