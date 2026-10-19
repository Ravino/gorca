# ADR-0024 fact-sources contract: adversarial design review (r1)

Date: 2026-10-19. Reviewer. Main at `6e09fdc6`. Scope: `docs/adr/0024-fact-sources-contract.md`
(Proposed), the ADR-0022 r3 note, `docs/plan/m22a-spec.md`, the `docs/plan/phase-3.md` diff
(M23–M25, M14a-core amendment) and `docs/reports/m22a-kickoff.md` (D-1..D-6). Cross-checked with
`semantic-fusion-math.md`, `adr-0022-review.md`, `chromium-base-design-review.md` and the code.
Read-only, no product code.

**Verdict: ACCEPT-WITH-CHANGES.** The shape is right: two transports and one contract, confidence
used for admission and hedging but never for ranking, non-primary sources off the key → speech
path by construction (LB-1, FS-6), O(1) masking. Two arithmetic errors make A1 fail its own gate
on day one (H-1, H-2). The trust model has four holes that a hostile page, add-on or same-uid
process can use (H-3..H-6). One plan step contradicts the spec's own dependency rule (H-7).
Fix those before A1 starts; the medium items can land as ADR r2 text before wave B.

## Evidence (commands and measurements)

- **Size probe** (rustc, `Semantics` modelled as `[u64; 8]` = 64 B, x86_64, scratch
  `/tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/sz/m.rs`):
  ADR-0022 r2 `SemEntry` = **120 B**; with `hedge: u32` = **128 B**; with `known/stale/fallback/hedge` packed into
  one `u128` (the ADR-0024 §2.12 fallback) = **128 B**, because `u128` has 16-byte alignment;
  with the masks packed as `[u32; 4]` = **128 B**.
- Group count: `semantic-fusion-math.md` §1.1 lists **18** fused groups (role, name, description,
  check, press, expand, select, current, invalid, popup, avail, flags, level, setpos, grid, live,
  text, existence). `Semantics` also carries `orientation` and `sort` (aria-semantics-architecture
  §3.1). `ProvTag: u64` at 4 bits per group holds 16 groups (math §2.6 says "16 groups × 4 bits"),
  and `hedge: u32` at 2 bits per group also holds 16.
- `crates/gorca-model/src/doc.rs:68-79`: `SrcNode` carries an already-mapped `role: Role` and
  `states: StateSet`. Mapping happens in `gorca-doc-source-atspi/src/roles.rs:82,249`, and
  interpretation in `gorca-core/src/doc_actor/translate.rs:31,194-271` (`web_attrs`).
- `crates/gorca-ext-api/Cargo.toml`: depends only on `gorca-i18n` and has no `gorca-model`.
  The WIT is still `gorca:ext@0.3.0` and has no `node-ref` or `prop-value` yet.
- `xtask/src/deps.rs:27-58`: `NEUTRAL` and `TOKIO_FREE` lists exist. There is no rule for future
  `gorca-source-*` crates.
- `docs/adr/` has no ADR-0016 (M14a-core depends on it).
- `gorca-doc-source-api/src/join.rs:8` `JoinQuality {Exact, Heuristic}` exists (the kickoff
  citation is correct).
- In-flight edits in worktree `agent-a508eca24e2626204` (`doc_route.rs`, `doc_actor/mod.rs`,
  `browse/latency.rs`) overlap the C2 files. That is consistent with "wave C after CHR-0", but
  not with any change to wave A/B scope.

## Findings (ranked)

### High

**H-1 The M-9 `SemEntry ≤ 112 B` gate cannot be met as specified.** (ADR-0024 §2.12, spec §6/A1)
The r2 layout is already 120 B, and adding `hedge` makes it 128 B. The prescribed fallback (pack
the masks into a `u128`) is also 128 B, because of `u128` alignment. A1's own `const` assertion
fails on day one, and the implementer will improvise.
*Fix:* choose now. Either (a) set the gate to 128 B (two cache lines either way; the 100k-node
cost is +1.6 MB versus 112 B), or (b) move `description` into `SemText` or one
`Option<Arc<NameDesc>>`, giving 104–112 B. Write the chosen layout into the A1 spec.

**H-2 The hot-path masks are sized for 16 groups, but the model has 18–20.**
`ProvTag u64` at 4 bits per group, `hedge u32` at 2 bits per group, and the math report's
"16 groups" do not cover the group list in math §1.1 plus orientation and sort. Groups 17 and up
have no provenance or hedge bits. `prov-info` is also planned to be exposed through WIT (§2.10).
*Fix:* freeze the group list in A1. Then either widen `ProvTag` to `u128` and `hedge` to `u64`
(which feeds H-1), or merge rarely-fused groups (orientation and sort into `flags`; `existence`
needs no ProvTag slot). Add a `const` assertion that `GROUP_COUNT * 4 <= ProvTag::BITS`.

**H-3 "Primary `Unknown`" mixes "cannot express" with "not fetched yet".** (§2.2 S0, D-4a)
A secondary wins "where the primary is `Unknown` (never fetched or not expressible)". *Never
fetched* is a transient state: focus masks are partial (`FOCUS_FIELDS` excludes DESCRIPTION,
`present/flow.rs:121`; ADR-0022 §2.3 falls back to lazy `ATTRS`), and the mirror fetches lazily.
Failure scenario: a T3 add-on fact for `check` or `expanded` wins on a node whose `ATTRS` have
not arrived, and the first focus utterance speaks it. When the primary fetch lands, the winner
changes to Observed-primary. LB-5 only silences changes whose *new* winner is non-primary, so the
change is announced ("not checked") although nothing changed.
*Fix:* a secondary may win only groups that the source profile's static `PI` rows declare
inexpressible for the primary. Dynamic `Unknown` never admits a secondary. Add a ST rule that a
change of winner class at equal value, or from secondary to primary, is silent. Add both to F12.

**H-4 The browser-bus scope is declared by the source and authenticated only by uid.**
(§2.6 `SourceDescriptor.scopes`, TP-1)
`SO_PEERCRED` "same uid" admits every process of the user, and `ScopeFilter` is self-declared.
Failure scenario: any local process (or a compromised add-on) connects as the bus. It asserts
`hint.title_attr` facts on `Doc`/`App` scopes it does not own, such as another tab, the browser
chrome or another application. Through the S3 title rule this relabels unnamed controls there.
*Fix:* the core pins each connection's scope from the transport. The native host's peer pid is
mapped through its ancestry to the browser process, then to that browser's `AppId`. `DocRef` is
derived by the host from (tab, frame) and checked against `DocMeta.url`; it is never taken from
the fact. A fact outside the pinned scope is a `Violation`. For WASM, the scope is the world
instance's scope. Write this as FS-11.

**H-5 Untrusted evidence is laundered into unhedged "Derived" values.** (§2.1, §2.2 S3,
kickoff §3 example 1)
`hint.title_attr` from a T3 source passes through S3 and becomes Derived/`Asserted`. It is spoken
plainly, outranks every Inferred value, and its `ProvCode` says `Derived`, so `explain` and the
hedge both lose the T3 origin. The S3 title rule fires exactly when the engine itself did not
compute a name from `title`, which is also the strongest spoofing signal.
*Fix:* S3/S4 outputs inherit the lowest tier and confidence class of their inputs (taint). Add
`ProvCode::DerivedSecondary` (there is room for 8 more codes). Apply the hedge and LB-5 to
tainted values. Extend F12 to cover them.

**H-6 Rebinding moves facts to a different element (wrong-node speech, IR-3).** (§2.3.2, §2.4)
"Binding is a function of the current mirror and the anchor (no history)" combined with "when the
primary removes the bound node, they unbind" means a `DomId` reused by a new element takes over
the old facts. That happens on SPA re-renders, in virtualised lists (`id="row-3"`) and with React
keys. FS-5 staleness applies to Inferred facts only. `DomId` uniqueness is checked on the mirror,
but the DOM can contain duplicates that are not in the a11y tree (hidden templates,
`aria-hidden`), and ids are scoped per shadow root and per frame.
*Fix:* a register stores the binding generation it was asserted against, and removal of the bound
node marks the fact stale until the source re-asserts it with a newer document revision. A
`Unique` binding needs uniqueness on both sides: the add-on reports the DOM count, including
shadow roots, and qualifies the id by shadow-host path and frame → `DocRef`. For the UI-redress
case: CHR-0's "act only on what was spoken" check on Enter must refuse when the spoken name's
winner was non-primary and its binding changed (generation or `sem_rev`) after speech.

**H-7 C2 contradicts the spec's dependency rule.** (spec §1 vs C2)
C2 converts `SrcNode → SrcRecord via gorca-sem-atspi` inside `gorca-core/src/doc_actor/`, but
§1 allows only `gorca-connector-atspi` and `gorca-doc-source-atspi` to depend on
`gorca-sem-atspi`. Besides, `SrcNode` already holds a lossy mapped `Role`, so the raw u32 role is
not available in the core.
*Fix:* run the normaliser in `gorca-doc-source-atspi` and give `SrcNode` a `rec: SrcRecord` field
(a `gorca-model` change). The change sits next to CHR-0 code, so schedule it in wave C and state
it in the C2 row. Then `translate.rs`'s `web_attrs` parsing (TAG, XML_ROLES, LIVE, LEVEL) moves
into the normaliser (SM-3/SM-4).

### Medium

**M-1 Wave B breaks the spec's own limits.** R1 (B1→B2), R2 (B3→B5), L1 (B4, a Rust crate) and
P1 (B6) make 4 implementers and **4 cargo builders**, while §4 allows at most 3. The "free fourth
slot for M14a-core" also does not exist in wave B. *Fix:* give B4 to R2 after B3 (or start B6
after B4), and fit M14a-core only into wave A or after wave B.

**M-2 Task sizes are optimistic.** B1 (registers, side table, seven strata, fill rules, hedge,
ProvTag, diagnostics, `explain`, differential tests and F1–F6, F9–F13) is planned at 2 days. C1
and C2 are 2 days each. Recent milestones needed 2–3 review rounds each. Expect a critical path
of about 18–20 days, not 12–13. *Fix:* split B1 into B1a (registers, strata, fill, ProvTag) and
B1b (S4/evidence, `explain`, diagnostics). Split C1 into C1a (concurrent `ATTRS` plus the M-6b
measurement first, because it is the only key-path change) and C1b (dual-carry writer).

**M-3 Scope creep against the owner decision.** The decisions row says "Only the contract is
*designed* in M22a". The spec builds the full runtime: `FactPort`, the state machine, the source
registry, the TTL wheel, the anchor index, the side table and the flood benches. None of it has a
real consumer until M23/M24, which rank after M19. *Option:* keep what is pure and what the WIT
freeze needs (value types, registries, S4 and evidence strata in `FUSE`, the fake source driving
registers directly) and move the `FactPort`/registry/state-machine runtime (part of B3, the flood
parts of B6) to an M23-0 prelude. That saves about 3 days and keeps the writer change minimal.
Recommend this, with medium confidence.

**M-4 The admission budget sits on the focused writer, and the gate is not measurable.** The
0.5 ms admission tick, the 2 ms FUSE batch budget and the revocation re-fuse chunks all run
inside the actor that also handles focus. M-14 (Δp99 ≤ 0.1 ms) is measured only at 5k facts/s;
M-15 (flood) only says "primary path unchanged". A Δ of 0.1 ms is below this host's noise
(load 11–13). *Fix:* admit in ≤ 50 µs slices and check the interactive lane between slices. Add a
K2 Δp99 number to M-15. Define the Δ gates as interleaved A/B, at least 3 runs, on a quiet host.

**M-5 The `FactPort` is not bounded.** "Memory bounded by distinct keys" does not bound anything
when the keys are attacker-chosen (random `DomId`s), and where the token bucket sits relative to
`offer` is unspecified. *Fix:* cap port entries (derived from `max_registers`), apply the bucket
in `offer`, and reach `Throttled` on the cap. Make the port lock-free or per-producer so that
"never blocks" holds.

**M-6 One hostile page can disable the bus for every tab.** One browser equals one source. Page
content longer than 1 KiB (a long `title`) is a `Violation`, and 3 violations in 60 s quarantine
the whole bus; buckets are per source as well. *Fix:* truncate oversize page-derived strings
instead of counting them, and keep strikes, buckets and quarantine per (source, scope) first.

**M-7 Quarantine can be evaded by reconnecting, and paused sources hold memory forever.** `Hello`
on a new connection creates a fresh instance (`Registered → Active`). `Paused` and `Quarantined`
have no `TransportClosed` transition, so their registers are kept until revocation. *Fix:* key
quarantine and pause by stable identity (descriptor name plus verified peer exe or extension id)
for the whole session. On `TransportClosed`, purge from those states.

**M-8 `SourceId` runs out.** It is a `u16` that is "never reused" and is allocated "per (add-on,
scope)" (§2.10). 65,536 allocations means 5 site add-ons × about 13k navigations, which a
multi-day session reaches. *Fix:* allocate per add-on instance and put the scope in the register
key, or use `u32` with a generation.

**M-9 Order between same-kind S4 instances is not defined.** With two add-ons or two OCR models,
tie-breaking by `SourceId` (allocation and load order) makes the result depend on run order.
*Fix:* break ties by static (kind order, `ext_id`, model id), as patches do.

**M-10 Revoked values stay speakable for up to 1 s.** The epoch bump is O(1), but published
entries are re-fused in background chunks, and the key path reads published entries. A source
quarantined for spoofing therefore keeps its names audible. Reading an `AtomicU64` inside `FUSE`
also makes `FUSE` impure (and F14 harder to test). *Fix:* in the revoking turn, re-fuse the focus
chain and the visible browse window synchronously. Deliver epoch changes to writers as timeline
messages.

**M-11 One bus feeds many writers.** The browser bus talks about documents owned by different doc
actors, but the ADR says "the writer drains it". *Fix:* specify one port per (source, writer) and
a router that resolves the pinned `DocRef` to its writer (see H-4).

**M-12 Privacy is enforced too late.** X-1..X-11 are checked at admission, after the DOM data has
already crossed the socket. *Fix:* the add-on enforces the exclusions at the source: never read
input values, password fields or `autocomplete=cc-*`; skip private windows and browser-internal
pages by default.

### WIT / ABI hazards for M18a

- **W-1** `facts.assert(subject: node-ref, …)` keys extension facts by node, which ADR-0024's own
  alternative E rejects: facts are orphaned on every re-render and on every CHR-0 rebuild. Make
  `subject` a WIT `variant` (node, dom-id, structural, region) in 0.9, including the cases M23/M24
  need now, because adding a case later changes the type.
- **W-2** If `prov-info.layer` and `.class` are WIT `enum`s, every later class is a breaking type
  change. Use `u8` registry codes.
- **W-3** The `GroupCode` semantic and evidence ranges are frozen at M18a. Split them by a bit
  (bit 15 = evidence) rather than by a fixed boundary.
- **W-4** `basis-ref` is undefined for extensions. Define it (for example as the host-derived
  `SemRev` of the inputs the hook read) or drop the parameter before 0.9.

### Low

- **L-1** `gorca-ext-api` has no `gorca-model` dependency, so `codes.rs` would duplicate
  `SemRole`/`GroupCode`. Name one source of truth and add an equality test.
- **L-2** Add an xtask rule: `gorca-core` must not depend on `gorca-source-*`.
- **L-3** A1 also edits the root `Cargo.toml` (workspace members), which is not in its owned files.
- **L-4** ADR-0016 does not exist; M14a-core's one architect day plus a review round is not
  counted in any wave.
- **L-5** D-1 refinement (optional): inside S4, ordering by (`ConfClass`, static order) is still
  order-independent (the class is a deterministic function of the fact and the grant) and lets a
  `High` image model beat a `Possible` OCR result. FS-4 would need a wording change.

## Owner questions D-1..D-6

| # | Reviewer view |
|---|---|
| D-1 | **(a)**, high confidence. Numeric fusion across sources is unexplainable and lets a confident wrong model override an author's label. Conditions: the taint rule (H-5) and the S4 tie-break (M-9). L-5 is optional. |
| D-2 | **(a)**, with the add-on enforcing secret-field exclusions (M-12) and private windows plus browser-internal pages off by default. |
| D-3 | **(a)**. The hedge must also apply to tainted Derived values (H-5); otherwise "plain" is not honest. |
| D-4 | **(a)**, but "primary `Unknown`" must mean statically inexpressible per source profile, never not-yet-fetched (H-3). |
| D-5 | **(a)** for waves A/B, after H-1/H-2 are fixed in the A1 spec and the wave B slots are corrected (M-1). Wave C after CHR-0 and P2-ARIA-0, with the H-7 C2 rewrite. |
| D-6 | **(a) only once ADR-0016 is written and accepted** and a real slot exists (wave A, or after B). Otherwise (b). This is the riskiest of the six for schedule, not for design. |

## Questions this review does not settle

- Whether CHR-0 changes `SrcNode` or `gorca-model/src/doc.rs` (that decides H-7 merge order).
- The real cost of the concurrent `ATTRS` fetch on Chromium (large `class` attributes); M-6b is
  the first gate to run.
- Whether the native-messaging peer pid can be mapped reliably to the browser `AppId` under
  Flatpak or Snap browsers (H-4); M23's first task should check this alongside TBD-S3.

## Delta review r2/r3 (2026-10-19, main `6bbea052`; commits f6ac24fd, 6bbea052)

**Verdict: A1 GO,** once one fixture inconsistency is fixed or the A1 test is told to ignore it (D-1
below). All of H-1..H-7, M-1..M-12, W-1..W-4 and L-1..L-5 are resolved in the ADR-0024 r3 text or
the spec (ADR §8 maps them, and I checked each location).

Evidence:
- `rustc research/semsize/layout.rs` prints `r2-16 104 | x32 128 | x32 with u128 ProvTag 128 |
  x32 + Semantics 80 144`, so x32 = 128 B fits under the 160 B gate.
- The `group_codes` in `tests/fixtures/fuse/fuse-x32.json` match the A1 order exactly (role = 0 ...
  sort = 17; evidence codes 32768..32774).
- Wave B has 3 builders. C2 now runs the normaliser in `gorca-doc-source-atspi` (H-7).
- The owner's answers (bdaf63db) match the spec: D-6 (b) leaves the wave A slot empty until
  ADR-0016 exists.

Remaining items:

| # | Sev | Where | Problem | Fix |
|---|---|---|---|---|
| D-1 | Low, blocks A1's test | `tests/fixtures/fuse/fuse-x32.json` `tables.prov_codes` (`tools/research/semantic_fusion.py:929`) | The table still lists `"Fallback": 8`. No case uses it, but the ADR and A1 removed `Fallback`, and A1 must "compare the header tables with the Rust registries", so the test fails or `Fallback` gets re-added | Drop it from the oracle table and regenerate both corpora, or have A1 skip unused codes ≥ 8 explicitly |
| D-2 | Medium, M18a | ADR-0024 §2.10 `value: prop-value` | `prop-value` is not defined anywhere (ADR-0019 and ADR-0022 only name it). Evidence needs a rect (`layout.box`), and future group slots 18..31 may need new value shapes. Adding a WIT variant case later is a breaking change | In 0.9, define `prop-value` with every shape the 32 slots and the evidence kinds need (bool, tri-state code, u32, string, rect, u32 list), or with a typed escape case |
| D-3 | Low | spec §3 C1a "runs as soon as CHR-0 merges, even before wave B ends" (R1 + P1) | R1 and P1 already build in wave B, so starting C1a early makes a 4th builder or needs a 5th implementer | Start it only in a free builder slot |
| D-4 | Low | fixture `max_str: 64` vs ADR 1 KiB truncation | This is an oracle scale parameter; B3 must not copy it as the product value | Label it in the schema header |
| D-5 | Note | `description` moved into `Arc<SemText>` | Every node with a description now allocates a `SemText`. `Semantics` equality must compare `SemText` by value, otherwise `sem_rev` churns | Make A1's derive `Eq` compare by value; the 100k-node RSS gate covers the memory |

Nothing else in the r3 text forces a later WIT break. `fact-target` has all of its cases now,
`prov-info` uses `u8` codes, and `GroupCode` uses bit 15 for evidence.

## M22a-A1 code review (2026-10-19, branch `worktree-agent-adb1bb7a7180aa6c5`, 93280c7f + 5888e8b9)

**Verdict: MERGE AFTER FIXES.** The fixes are small (F-1 must land; F-2..F-4 can go in the same
commit).

What I ran (own target dir):
- `cargo fmt --check`: OK.
- `cargo test` for `gorca-model`, `gorca-ext-api`, `gorca-fuse`, `gorca-fact-api` and
  `gorca-sem-atspi`: all pass (33 + 13 + 4 + 4 tests). The size test asserts `SemEntry` = 128 B.
- `cargo xtask deps`: OK (40 crates). `cargo test -p xtask`: 19 passed.
- `cargo clippy --workspace --all-targets -D warnings`: clean.
- `git merge-tree` against main `311a5592`: clean.
- No `unwrap`, `expect` or `panic` outside tests.

I confirmed each of the claims I checked: layout x32, the `const` asserts, `ProvCode` without
`Fallback`, the fixture diffs being limited to `Fallback` and the header label, rule 10 plus
`core_forbidden` blocking `gorca-sem-atspi` and `gorca-source-*`, and `SemText` comparing by
contents.

| # | Sev | Where | Problem (verified) | Fix |
|---|---|---|---|---|
| F-1 | Medium (WIT stability) | `tools/gen-sem-codes.py`, `crates/gorca-ext-api/tests/codes.rs` | The append-only guard does not protect anything. The generator **rewrites** `codes.snapshot` from the current tables. Probe (in a scratch copy): renumber `SemRole` button 8 → 9 and caption 9 → 8 with the lines reordered, rerun the generator, and **all `gorca-ext-api` and `gorca-model` tests pass**. Only a reviewer reading the snapshot diff would notice | Make the generator append-only: read the existing snapshot and exit non-zero if any row changed or disappeared. Also add a CI or xtask check that `git diff <merge-base> -- codes.snapshot` contains only `+` lines |
| F-2 | Low-medium (M18a) | `sem/states.rs`, `SemFlags` | The tri-state codes (`Check`, `Press`, `Expand`, `Select`, `Current`, `Invalid`, `Popup`, `Avail`, `NameFrom`, `Orientation`, `Sort`, `PosSrc`) and the `SemFlags`/`Relevant` bit values will be wire values inside `prop-value`, but they are not in `REGISTRIES` or the snapshot | Add them to the registries and the snapshot now, while it is cheap |
| F-3 | Low | `sem/role.rs` | The doc comment says "ARIA 1.2 + graphics-aria + DPUB subset". None of the DPUB or graphics roles are present, nor the ADR-0022 r2 additions (`Comment`, `Suggestion`, `Details`, `Summary`, `DescriptionList`, `Time`, `Iframe`), nor ARIA 1.2 `emphasis`, `strong`, `subscript`, `superscript`, `none`/`presentation`. Appending later is safe because codes are append-only | Append the r2 additions in A1, as the spec requires, or correct the comment and leave the rest to B4 (M-10 coverage) |
| F-4 | Low | `sem/codes.rs` `coded_enum!` derives `Ord` | `ConfClass` and `TrustTier` sort with lower code = stronger, but `BindQuality` sorts with higher code = stronger. A future `class >= floor` written like `quality >= min_bind` is silently inverted | Drop `Ord`/`PartialOrd` from `ConfClass` and `TrustTier` (keep `weaker()` and add a `stronger_than`), or document the direction on each type |
| F-5 | Low | `fact.rs` `PropValue::matches` | Any `Evidence` payload matches any evidence kind: `Flag(true)` passes for `hint.title_attr`. The helper `evidence_is_text_kind` exists but is not used, so a malformed fact would pass the well-formedness check | Check the payload shape per kind in `matches` (B3 needs this for admission) |
| F-6 | Note (for B1b) | `SourceId` derives `Ord`; ADR §2.2 S4 tie-break "(…, source id, …)" | Slots are recycled, so `SourceId` order depends on allocation order, and run-to-run results would differ (my M-9) | Break S4 ties by `StableIdentity` name or model id, not `SourceId` |
| F-7 | Nit | `gorca-sem-atspi/src/lib.rs:5` | Says "Empty until M22a-B2"; the normaliser is task B4 | Fix the comment |

Accepted deviations: `gen` → `generation` (`gen` is a reserved keyword in edition 2024) and
`SemRole::coarse()` deferred (SM-1 lands with the writers). `gen-sem-codes.py` reads the oracle
fixture rather than `gorca-model`, but the two equality tests keep all three in step.
