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

## M22a-B3 code review (2026-10-19, branch `worktree-agent-a3ee8ca2ee852bdf6`, 6c760dcd)

**Verdict: MERGE AFTER FIXES.** Three state-book defects, each confirmed with a probe test, plus
two small items. The pure contract is otherwise solid.

What I ran (own target dir):
- `cargo fmt --check`: OK.
- `cargo test` for `gorca-fact-api`, `gorca-testkit` and `gorca-model`: all pass (11 + 108 + 36
  tests, 8 ignored).
- `cargo xtask deps`: OK. `cargo clippy --workspace --all-targets -D warnings`: clean.
- `git merge-tree` against main `90b3ed4f`: clean.
- No `unwrap`, IO, clock or tokio in the crate.

What holds:
- `step` matches every (state, event) pair exhaustively, without wildcards.
- `Hello` takes its memory from the book, so a reconnect under the same identity cannot escape.
- `SourceId` has a generation, so a recycled slot gets a new epoch key.
- Node-subject scope pinning works, and a `dom_count != 1` id never binds `Unique`.

Probe file (kept): `/tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/probes/audit_b3.rs`,
run as `crates/gorca-fact-api/tests/audit_b3.rs` in a `git archive` copy.

| # | Sev | Where | Problem (verified) | Fix |
|---|---|---|---|---|
| B-1 | Medium | `state.rs` `note_quarantine` + `admit.rs` `PrivacyExcluded` severity `Violation` | Per-tab isolation is defeated. A scope is `(app, doc)`, so every reload of a hostile site creates a new scope. Three quarantined docs within 10 minutes quarantine the whole bus. **Probe: three doc scopes quarantined → an innocent tab (doc 100) becomes `Quarantined`.** A page can cause the violations itself: a "show password" toggle flips `type=password` between the add-on's send and admission, which gives `PrivacyExcluded`, which is a strike | Make `PrivacyExcluded` (and admission-time races such as `ScopeMismatch` for a node whose doc changed) `Benign`, counted but not a strike. Count source-wide quarantines over distinct sites or origins (or apps), not distinct doc ids |
| B-2 | Medium | `descriptor.rs` `StableIdentity { name, peer }` | `name` comes from the source's own descriptor. **Probe: a reconnect with name `bus2` under the same `PeerProof` starts `Active`.** That escapes M-7 | Key the memory by `PeerProof` alone, or take `name` from a verified manifest, never from `Hello` |
| B-2b | Medium (security review, concurred) | `descriptor.rs` / `SourceBook` | Same root cause as B-2. Session memory (quarantine, revocation, wide quarantine) must be keyed by the verified `PeerProof` (or a manifest name the core verified). Where several identities share a `PeerProof`, the strongest memory among them applies | Key by `PeerProof`; look up `max(remembered)` across names |
| B-6 | Medium (security review, concurred) | `admit.rs` step 1 | The scope check fails open: when `lookup.node_scope(n)` returns `None`, a `Node` subject of the pinned app passes the check. It is safe today only if `bind()` also returns `Unbound` for the same node, and the contract does not require the two lookups to agree | Fail closed: if `node_scope` is `None`, park the fact (or reject it as `Benign`) and re-run `admit` once the scope is known. Add a kit case where `bind` knows the node but `node_scope` does not |
| B-3 | Low-medium | `SourceBook::apply` `Forget` | A user `Resume` clears only the scope's memory; `wide_memory` stays. **Probe: after a wide quarantine, a user Resume, then a reconnect, the scope is `Quarantined` again.** The user's decision is undone | When the user resumes, clear the wide memory too (or require one explicit "re-enable source" that clears both) |
| B-4 | Low | `SourceBook` maps | `states`, `scope_memory` and `quarantines` are keyed by `(identity, scope)` with doc scopes and are never evicted, so they grow with every navigation over a session (bounded, but monotone) | Evict `Detached { purged: true }` entries that have no memory; cap the quarantine log |
| B-5 | Low | `ScopeLookup::bind(&Subject)` | Anchor subjects get no scope argument. A writer whose implementation uses a per-app `DomId` index would bind across tabs, which the pinning was meant to prevent | Pass `pinned: Scope` to `bind`, or document that one lookup is built per pinned scope and test it in the kit |

Rulings on the stated deviations:
- `SourceGrant.anchors`: accept.
- Well-formedness checked before binding: accept, so parked facts are always valid.
- Inferred without TTL → `MissingTtl` as a violation: accept (the TTL is required by the ADR).
- `Detached` + `PrivacyPause` → stay `Detached` and remember `Paused`: **accept**. The facts are
  already masked, and the pause applies on the next `Hello`. Add this to the ADR §2.6 table.
- T3 cap applied at admission: accept, on condition that B1b ranks and hedges only by
  `Admitted::class`. The floor stays on raw `p_milli`, as in the ADR.

## M22a-B1a code review (2026-10-19, branch `worktree-agent-a5fa450750973b3b7`, d0b45ce5)

**Verdict: MERGE**, with two follow-ups that must land in B1b (N-1, N-2).

What I ran (target dir `/var/tmp/gorca-review-target`):
- `cargo fmt --check`: OK.
- `cargo test -p gorca-fuse`: 27 unit tests, 2 oracle tests (30 cases × 4 deliveries, both
  corpora) and 10 property tests, all passing.
- `cargo xtask deps`: OK. `cargo clippy --workspace --all-targets -D warnings`: clean.
- `git merge-tree` against main `bd998f5d`: clean.

What holds:
- No `HashMap`, `SourceId`, tokio, IO, clock or `unwrap` outside tests.
- The registers are join-semilattices with a total tie-break, consistent with Lemma 1.
- H-3 holds: a Secondary is consulted only when every primary is `Unknown` **and** the group is in
  the static `inexpressible` mask. A unit test covers an expressible group being ignored.
- The fill rule admits only `Observed` and `Default` layers, so a Secondary, Patch, User, Derived
  or Inferred value is never overwritten.
- Name from content takes untainted children first and marks the result `DerivedTainted` with the
  weakest class and least trusted tier (math §10.4 item 1).
- The T3 cap is applied again inside FUSE.
- S0 ranks Secondaries deterministically by a stable `src_key` that is never a `SourceId`.

| # | Sev | Where | Problem | Fix |
|---|---|---|---|---|
| N-1 | Medium (B1b gate) | `strata.rs` `FactCand` / `NodeCands` (all fields `pub`) | Validity (binding minimum, `BindGen`, epoch, `vis_rev`, TTL, floor/standing) is only a documented caller duty. Once B1b lands, any writer can build `NodeCands` straight from raw side-table rows and FUSE will accept them, so H-6, FS-5 and F15 depend on discipline | In B1b, make the candidate type constructible only through the side table's validity filter (private fields and a `pub(crate)` constructor, or a sealed `ValidCands` produced by `side::candidates(..)`), so `fuse_node` cannot receive an unchecked fact |
| N-2 | Low-medium | `strata.rs` S0 Secondary, S3 title, S4 | `Info.conf` is taken from `FactCand.conf` with only the T3 cap applied. A non-primary candidate carrying `ConfClass::Asserted` (a bug in B1b, or a T2 tier) is spoken plainly even though its provenance is `ObservedSecondary`/`Inferred`/`DerivedTainted` | Cap non-primary winners at `High` or weaker in FUSE (`conf.weaker(High)`), which is redundant with B3's `class_of` but defensive. Add a unit test |
| N-3 | Note | `tests/support` `cands_from_expected` | The differential test takes non-primary candidates (and their order) from the oracle's own expected trails. The 120 runs fully verify the primary path, but for Secondary/S3/S4 they verify only selection given the oracle's filtering and ranking | B1b's differential run must compute candidates from raw facts through the side table and compare the trails |
| N-4 | Low | `profile.rs` `RoleSet(u128)` | A role code ≥ 128 is silently dropped by `with`. The `const` assert `SemRole::ALL.len() <= 128` turns that into a build failure once roles grow, and A1's F-3 additions plus DPUB (about 40 roles) will cross it | Widen to `[u64; 4]` now, or plan for it in B4 |
| N-5 | Note | `strata.rs` `fuse_node` | `known` is computed before repair, so a masked group (for example `Absent` check on a role that is not checkable) stays `known` with `Default` provenance. This matches the oracle (the stated fidelity quirk), but the ADR text does not say it | Record it in ADR-0024 §2.2 R, or in math §10, as intended behaviour |

Taint propagation is correct for the paths B1a owns: a Secondary or title winner carries its own
tier, and content names carry the least trusted tainted child. A primary's taint is encoded as 0
(`T0Core`), not T1, which matches the oracle's "0 = untainted" convention.

## M22a-B4 code review (2026-10-19, branch `worktree-agent-ab70e69616d122fa3`, df92bbb0)

**Verdict: MERGE AFTER FIXES.** P-1 is a behaviour regression to fix in this branch. P-2 and P-3
are small.

What I ran (target dir `/var/tmp/gorca-review-target`):
- `cargo fmt --check`: OK.
- `cargo test -p gorca-sem-atspi`: 45 unit tests plus the corpus, evidence and sweep tests, all
  passing.
- `cargo xtask deps`: OK (rule 10 holds; the crate depends only on `gorca-model`).
- `cargo clippy --workspace --all-targets -D warnings`: clean.
- `git merge-tree` against main `fb2f5db2`: clean.

What holds:
- No `unwrap`, `HashMap`, tokio or IO outside tests.
- Matches over roles, states and `RoleSource` are exhaustive.
- The output is a deterministic function of the input (attributes are parsed by key; the first
  valid `xml-roles` token wins).
- Unknown vs Absent follows the fetched mask.

| # | Sev | Where | Problem | Fix |
|---|---|---|---|---|
| P-1 | Medium | `record.rs` invalid block (`required && a.is_web()` → `Invalid::No`) | This drops a real validation error. `INVALID_ENTRY` on a REQUIRED web field with no `invalid` attribute is always discounted, including a **non-empty** field that fails constraint validation (`type=email required` containing "abc"). The same field without `required` is still announced as invalid. The legacy path (`gorca-doc-source-atspi/src/roles.rs:278`) maps `INVALID_ENTRY` → `INVALID` unconditionally, so this also produces a legacy-agreement disagreement in C5 | Keep the fact (`Invalid::True`) and add a `Notes`/flag bit such as `INVALID_FROM_CONSTRAINT` so presentation can suppress only the empty-required case. Alternatively discount only when emptiness is known (unchecked checkbox; empty text once the value is an input). Add a corpus case `required + type=email + bad value` |
| P-2 | Low-medium | `record.rs` / `attrs.rs` | Page strings are not capped: name, description, placeholder, keyshortcuts, valuetext and the index texts are copied whole (only roledescription at 64 scalars and the tag are capped). A 1 MB `aria-label` passes through every register, the oracle tie-break text and the cache | Cap in the normaliser (for example name and description 4 KiB, the others 1 KiB), on a scalar boundary, with a `Notes::TRUNCATED` bit |
| P-3 | Low | `SrcRecord.index_text` | `FUSE` writes `Semantics`/`SemEntry` only, so these values have no path to presentation and are dropped at fusion | Add `col_index_text`/`row_index_text` to `SemText` now (struct field, not a WIT code; part of the `grid` group) and drop `SrcRecord.index_text` |

Rulings on the judgement calls:
- **(a) Tag refinement (code, time, mark, figure, fieldset, em, strong): keep it in the
  normaliser.** It is the HTML-AAM mapping, keyed on the exported `tag` attribute and a weak engine
  role, not on engine identity, so it is family knowledge (SM-4) and not an engine quirk. Rule for
  B5: anything keyed on *which* engine or toolkit (`is_gecko`, `is_blink`, version) goes into quirk
  rows. Example: `READ_ONLY` = missing `EDITABLE` (states.rs:349, "Gecko convention") is fine only
  while every engine sets `EDITABLE` on editable fields; if one does not, it becomes a quirk row.
- **(b) Password expectation: fix it at the source.** Change `expect.toml` (`textbox.password`,
  `gtk4.entry.password` → `passwordbox`, with a note) and delete `MODEL_ROLE_OVERRIDES`. An
  override table in a test lets later mismatches hide.
- **(c) Index texts: see P-3.** Move them into `SemText`.

## M22a-B1b code review (2026-10-19, branch `worktree-agent-a5fa450750973b3b7`, b8e212c2 + 5c2610a5)

**Verdict: MERGE AFTER FIXES.** The validity filter is sealed on its output side but not on its
input side (Q-1). Q-2 and Q-3 are fail-open defaults.

What I ran (target dir `/var/tmp/gorca-review-target`):
- `cargo fmt --check`: OK.
- `cargo test -p gorca-fuse`: 38 unit, 2 oracle (built from raw facts), 16 property and 2
  `compile_fail` doctests, all passing.
- `cargo xtask deps`: OK. `cargo clippy --workspace --all-targets -D warnings`: clean.
- `git merge-tree` against main `df64429f`: clean.
- No tokio, IO, clock or non-test `unwrap`.

What holds:
- `FactCand`, `Entry` and `Ranked` have private fields (N-1 output side).
- Non-primary classes are clamped with `weaker(High)` (N-2).
- S4 ranking is (class after cap, kind order, stable name, direct before rule, value, anchor,
  seq), never `SourceId` or raw `p_milli`.
- `bind()` sorts `HashMap` registers by (name, anchor, group) before use.

| # | Sev | Where | Problem | Fix |
|---|---|---|---|---|
| Q-1 | Medium | `side.rs` `FactIn` (all fields `pub`: `class`, `floor_ok`, `min_bind`, `bind_gen`) | **No: the API does not make `Admitted` the only path.** The filter trusts caller-asserted inputs. A writer that builds `FactIn { min_bind: Unbound, floor_ok: true, class: High, .. }` gets past the binding minimum and the floor (F11, F15) without touching `admit()` | Derive everything except `bind_gen` inside the side table. Register the grant data per source in `SourceInfo` (tier, per-group-class floors and `min_bind`, thresholds), compute `class` (with the T3 cap) and `floor_ok` from `p_milli` in `status_of`, and drop those fields from `FactIn`. `bind_gen` stays writer-stamped by design (math §10.4 item 5). Do **not** make `gorca-fuse` depend on `gorca-fact-api`: M23-0 adds `gorca-rt` there and `gorca-fuse` is `TOKIO_FREE` |
| Q-2 | Medium | `side.rs` `EpochSnapshot::state` → `unwrap_or(SourceState::Active)` | This fails open. A source missing from the snapshot (a forgotten `set`, or a state change racing the snapshot) is treated as Active, so a quarantined or revoked source's facts can win | Default to `Masked`/`Detached` (fail closed), or have `bind()` require that every source with registers is present in the snapshot. Add a test |
| Q-3 | Medium (ruling) | `status_of` `Basis::Capture(..) \| Basis::Snapshot(_) => false` | **Not acceptable as never-stale.** FS-5 says an inferred fact never outlives its basis, and ADR §2.4 makes a capture fingerprint mismatch stale. With no owner check wired, an OCR or extension name would survive any re-render until its TTL (5 min). No producer exists in M22a, so fail closed now | Either reject these bases at `insert` (`SideError::UnsupportedBasis`) until M18a/M24 add their checks, or record the node's `vis_rev` at admission and treat any bump as stale (conservative). Add an F13 case |
| Q-4 | Low | `side.rs` `key_order` / ranking ties on the stable name | Two `SourceId`s with the same stable name (a reconnect while the old registers sit in Detached grace) tie on every key, so the order of their entries, and the `explain` trail, follows `HashMap` iteration | Add a final tie-break on (generation recency, or `SourceId`) **after** all semantic keys (it cannot change a winner's value, only trail order), or purge on re-attach |
| Q-5 | Note | `build_entry` | An Inferred *semantic* fact for a group other than name/description is ranked into `inferred` but S4 consumes only name/description, so it is silently inert | Mark it `not-applicable` in the trail (or reject it at admission) so `explain` says why |

## M22a-B5 code review (2026-10-19, branch `worktree-agent-ab70e69616d122fa3`, dca83c87)

**Verdict: MERGE AFTER FIXES.** One data fix (R-1) blocks; the rest are low.

What I ran (target dir `/var/tmp/gorca-review-target`):
- `cargo fmt --check`: OK.
- `cargo test` for `gorca-fuse` and `gorca-sem-atspi`: all pass (51 + 17 + 48 + corpus and quirk
  tests).
- `cargo xtask deps`: OK. `cargo clippy --workspace --all-targets -D warnings`: clean.
- `git merge-tree` against main `ace58a38`: clean.

What holds:
- The loader is data-only: closed actions; unknown keys, actions, groups, roles and sources are
  rejected; ids are non-zero and unique, including across `extend`.
- The size caps work: 64 KiB per document, 256 rules, 32 prefixes of 16 bytes, 64 rows, 128-byte
  labels.
- The glob is iterative, uses no allocation, and costs O(text × pattern) with patterns ≤ 128
  bytes, so it cannot blow up.
- Rules apply in file order, and profile selection is first-match, so both are deterministic.
- The `role` group is refused as inexpressible.

| # | Sev | Where | Problem | Fix |
|---|---|---|---|---|
| R-1 | Medium (H-3) | `inexpressible/{gecko,blink}.toml` row `invalid` | `invalid` **is** expressible: both engines export `INVALID_ENTRY`, and the B4 normaliser marks the `invalid` group known (True/No) whenever `STATES` was fetched (`record.rs`, `known.set(Group::Invalid, true)`). Only the *kind* (spelling/grammar) is missing. The row therefore cannot add the kind (a known primary always wins S0). It **does** reopen exactly what H-3 forbids: a T3 secondary wins `invalid` in the window where `STATES` has not been fetched yet, and is then replaced by the primary | Delete both rows (leave the profiles with no inexpressible groups). If the kind is wanted later, model it as its own group (`invalid_kind`, appended registry code), which really is inexpressible, or as a tainted S3 refinement that applies only when the primary says `Invalid::True`. Add a loader or test check that every inexpressible group is one the profile's normaliser never marks `known` |
| R-2 | Low | `quirks/profiles.toml` `app_name` globs | The AT-SPI app name is chosen by the app, so any process named `firefox-x` or `chrome*` gets that engine's profile (quirk rows and inexpressible mask). The effect is bounded today (prefix strip, setpos Unknown) but grows with every row | Prefer the connector's `web_engine` and `toolkit` detection rows first (they are listed first already), and treat `app_name` rows as a fallback, preferably with the exe identity once ADR-0016's matcher exists. Move to the shared `gorca-config` glob when M14a lands (the stated risk) |
| R-3 | Low | `quirk.rs` `UnknownGroup` | A quirk row can turn any group `Unknown` dynamically. Combined with an inexpressible row for the same group, it would admit Secondaries per role, which is a second route around H-3 | Have the loader reject an `unknown-group` rule whose group is in the same profile's inexpressible mask (or document that the two must never overlap and test it) |
| R-4 | Note | `gecko.toml` rule 1 strip-name-prefix | This is the legacy X1 behaviour: an author's name that really starts with "• " on a list item is also stripped | Accept; it matches legacy, so the agreement gate is unaffected |

## M22a-B2 code review (2026-10-19, branch `worktree-agent-a5fa450750973b3b7`, 2a30182b)

**Verdict: MERGE AFTER FIXES.** The results are correct (F7, F14 and budget independence hold).
The scheduler has two latency and fairness defects (S-1, S-2), both shown by probes.

What I ran:
- Built in a temporary detached worktree of the merge with main `e14ad1be` (B5), at
  `/var/tmp/gorca-review-b2`, removed afterwards; target dir `/var/tmp/gorca-review-target`.
- `git merge-tree`: clean.
- `cargo fmt --check`: OK.
- `cargo test -p gorca-fuse`, with and without `--all-features` (`fuse_full_check`): 55 unit,
  15/14 ivm, 2 oracle, 17 property and 2 doc tests, all passing.
- `cargo xtask deps`: OK. Workspace clippy and `gorca-fuse --all-features` clippy with
  `-D warnings`: clean.

What holds:
- The dirty set is deduplicated per node (`dirty_of`), so it is bounded by the node count, and the
  background queue is deduplicated (`in_background`): no unbounded growth.
- Silencing states (Paused, Quarantined, Revoked, Detached) drain the priority class synchronously
  (FS-7).
- Ordering is deterministic (a `BTreeSet` key `(class, Reverse(depth), idx)`).
- The only clock is `Instant` in `Budget`, at scheduler level; it changes latency, never results.
- No tokio and no non-test `unwrap`.

Probe file: `/tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/probes/audit_b2_ivm_append.rs`
(append to `tests/ivm.rs`; run with `--release`). The host was loaded (load average 13.7), so
read the timings as orders of magnitude.

| # | Sev | Where | Problem (measured) | Fix |
|---|---|---|---|---|
| S-1 | High (key → speech p99) | `ivm.rs` `set_focus_chain`/`set_visible` → `rekey()` (re-keys **every** dirty node, each with an ancestor walk in `is_priority`); `bump_global` likewise | A focus change on the writer turn costs O(dirty × depth). Probe (release, star tree): **5,000 dirty nodes → 0.59 ms; 50,000 dirty → 9.7 ms for `set_focus_chain`**. The synchronous part of `bump_global` took **12.6 ms** at 50k (ADR M-16 allows ≤ 0.5 ms). During a storm, a keypress that moves focus would spend most of the 15 ms budget re-sorting the backlog | Keep the priority class separate: on a focus or visible change, compute the new priority set (focus chain, visible window and their content-feeding descendants, ≤ `max_children` each) and move only *those* members of `dirty_of` into class 0 (O(priority set)). Demote the old priority nodes lazily. Bound the synchronous drain in `bump_global`/`set_source_state` (ADR: ≤ 200 nodes, ≤ 0.5 ms) and carry the rest. Add a B6 gate: focus change with 100k dirty → Δ ≤ 0.1 ms |
| S-2 | Medium | `run()` ordering (deepest first inside class 1; background only when `dirty` is empty) | Starvation. Probe: a chain of 50 nodes; node 1 has a pending change; the leaf is re-dirtied before every `run(Budget::calls(1))`. **After 200 runs node 1 was never fused** (pending = 1). Under a sustained storm whose rate exceeds the budget, shallow nodes (ancestors whose content names change) and every background overflow dependent wait forever | Add aging: give each dirty entry an enqueue tick and, every k picks (or once its age passes a threshold), take the oldest. Give the background queue a guaranteed share of each budget (for example 1 chunk per run) instead of "only when `dirty` is empty" |
| S-3 | Medium (B6 to measure) | `reindex()` re-resolves **every** cached anchor on any lifecycle or DOM-id input | In a web storm (children added and removed every batch) with N anchors this is O(N) per batch. That is acceptable while N is small, but M23 with thousands of `DomId` facts makes it the per-batch hot path | Re-resolve per key: DOM-id adds and removals touch only the anchors of that id (`dom` index → anchors), and lifecycle touches only the anchors bound to the removed or added node (`anchor_nodes`) |
| S-4 | Note | Decisions | `vis_rev` writer-supplied: accept. Region, Structural and Content anchors left unresolved (Unbound → parked): accept for M22a (no producer); M23/M24 must add them | — |

## gorca-fuse perf round review (2026-10-19, branch `fuse-perf`, head 742ddec8, main b3e40f16 merged)

**Verdict: MERGE**, with two follow-ups (T-1, T-2) and one decision row (T-3).

What I ran (target dir `/var/tmp/gorca-review-target`):
- `cargo fmt --check`: OK.
- `cargo test -p gorca-fuse`, with and without `--all-features` (`fuse_full_check`): 63 unit,
  30/29 ivm, 2 oracle (both fixtures × 4 deliveries), 18 property and 2 doc tests, all passing.
- `cargo xtask deps`: OK. Workspace clippy and `gorca-fuse --all-features` clippy with
  `-D warnings`: clean.
- `git merge-tree` against main: clean.

**Is the S0 delta exact?** Yes, argued below and covered by tests.
- The delta form is exact over the selector-read mask: `delta_of` stores every group in the mask
  where `rebuild_slot(entry)` differs from the real S0.
- Change detection compares the *old view*, exactly what neighbours read, with the new S0. So any
  read made through an inexact view (a group outside an older mask) is re-dirtied when the node is
  re-fused, and the run converges.
- `Ivm::verify` checks view == real S0 over the mask for every node, and the F7 test calls it after
  every drained batch on random workloads, with patches installed and removed.
- Contract to document: `PatchSource::reads()` must cover every group a selector reads; a
  debug-assert in `ToyPatches`-style sources would enforce it.

**Mutation set** (fresh `git archive` copy per mutation, script at
`/tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/probes/mut_fuse_perf.sh`):

| Mutation | Result |
|---|---|
| M1: `delta_of` never records exceptions | caught; the ivm binary **hung** (20 min at 100 % CPU, killed) |
| M2: `changed_vs` never reports a change | caught |
| M3: register join ignores the value tie-break (Absent-run tie order) | caught |
| M4: value overwrite leaves the group in its Absent run | caught |
| M5: `rebuild_slot` ignores `known` | caught |

Also verified:
- Absent runs keep the per-group `(seq, value)` join, with Absent = "-" in the tie order, and a
  canonical store.
- The dirty heap is rebuilt when stale entries exceed 2 × live + 1024, so its growth is bounded.
  Aging comes from a first-dirty FIFO.
- `FxHasher` is used only for `NodeIdx` keys; page strings keep SipHash.
- The silence mask (`entry_view`) masks silenced contributors at read time, and F14 passes.
- No tokio and no non-test `unwrap`.

| # | Sev | Item | Judgement |
|---|---|---|---|
| T-1 | Low (follow-up) | Sync drain (chain + 200 fusions) p99 **530 µs** vs 0.5 ms (p50 247, p99.9 3.6 ms), on a loaded host (load 11, one pinned CPU) | **Acceptable for merge.** It runs only on source state changes (silencing), not per keypress, and the per-keypress focus gates pass (sync part p99 49 µs, chain fused p99 259 µs). Follow-up: make the drain bound time-based as well (stop at 0.4 ms and leave the rest in front for `run`), then rerun on a quiet host (`bench/b1/quiet-host.sh`). The p99.9 of 3.6 ms is the number to watch |
| T-2 | Low (follow-up) | S-3 per-key re-resolution ratio 2.03 (527 → 1073 ns for 10× anchors; worst p99 2.3 µs/key) | **Acceptable.** That is far from O(N): 10× anchors cost 2× per key (hash and cache effects), which is within noise of the ≤ 2 gate. Restate the gate as an absolute bound (p99 ≤ 5 µs per key at 20k anchors) plus a ratio ≤ 3 |
| T-3 | Process | Restated gates: M-9' 64 MB per 100k (realistic world) replaces 25 MB; M-11a per-event p99 ≤ 10 µs replaces ≤ 2 µs | Defensible (owner: memory not a hard limit; the old M-11 was a batch average), but **record a `docs/decisions.md` row** before merge so the gates are not silently moved |
| T-4 | Low | M1 showed that a consistency bug makes the engine re-dirty neighbours forever (a livelock under `Budget::UNLIMITED`; under a real budget it would burn CPU indefinitely) | Add an oscillation guard in debug builds: count re-fusions per node per drain and `debug_assert` a bound (for example 64), so such bugs fail fast in tests |

## M22a-C1a code review (2026-10-19, branch `worktree-agent-ae1e5a6f5402c52c9`, 328b53df)

**Verdict: MERGE AFTER FIXES.** U-1 is a tail-latency hazard on the key → speech path. U-2 and
U-3 are small. The rebase only needs the trivial `docs/decisions.md` conflict resolved
(`git merge-tree` against main `3cacb8a3`; it is the only conflicting file).

What I ran (target dir `/var/tmp/gorca-review-target`):
- `cargo fmt --check`: OK.
- `cargo test` for `gorca-connector-atspi` and `gorca-core`: 158 + 867 + integration tests, all
  passing.
- `cargo xtask deps` (including the environment rule): OK. Workspace clippy with
  `-D warnings`: clean.
- I did not rerun the e2e M-6b runs. The committed raw rows support the stated deltas (GTK4 ≤ +0.05,
  Chromium +0.3, Firefox 0.0 ms; K1 Δ < 0.04 ms).

What holds:
- The attribute filter is an **allowlist** (`decode::attr_key`, 19 known keys). `class`, `id`,
  `data-*` and styling are never kept; there are at most 32 values, each clipped to 256 bytes.
- An error reply counts as an empty answer; a timeout leaves `ATTRS` unset, so the node is asked
  again.
- The environment switch is read only in `gorca-daemon/src/main.rs` (xtask environment rule), and
  only the value `"1"` enables it.
- The partial hit (`Partial { missing: ATTRS }`) speaks only what the mirror already had (the same
  as before C1a), tags `may_refine` and revalidates. No value is invented, so stale or wrong
  speech can only come through the existing refine path (F17 is unchanged).

Changed reader tests (probe: I reverted the `+2 s` timings in a `git archive` copy):
- `a_stale_name_is_fetched…`: still passes at `+1 s`, so that change was **unnecessary** but
  harmless. Revert it to keep the diff honest.
- `a_partial_node_missing…`: at `+1 s` the slider focus becomes `["timer"]` (rapid-focus hold),
  because the new `Bare` step is also at `+1 s`. Moving it to `+2 s` is **justified**; it does not
  weaken the test.
- The fixture's default `have` gaining `ATTRS` is justified: `FOCUS_FIELDS` now includes it.

| # | Sev | Where | Problem | Fix |
|---|---|---|---|---|
| U-1 | Medium | `fetch/strategy.rs` `fetch_node` (`tokio::join!` of 7 calls under one deadline) | `GetAttributes` is optional for speech, but `join!` waits for it. If it is slow and the required calls are fast, the first utterance waits up to the whole Interactive deadline (`gorca-rt` `BudgetClass::Interactive` = **120 ms**). Gecko's `GetAttributes` computes layout-dependent attributes (`display`, …), so a heavy page or the known hung-tree cases (`<input type=date>`, report "Open") are the realistic trigger. The 150-input test page cannot show this | Do not let `ATTRS` gate the fetch result. Once the required futures are done, give `attrs` a short grace (a few ms) and otherwise return without `ATTRS`, so the node is `Partial`, `may_refine`, and revalidation fills it. Add a fake-app test where `GetAttributes` sleeps for 100 ms and assert the snapshot arrives within the grace |
| U-2 | Low | `caller.rs` `attributes_of` → `HashMap` → `into_iter()` | The attribute order is randomised per call (std `RandomState`), so two fetches of the same node give `AttrMap`s that differ only in order. Ordered comparisons exist (`doc_actor/translate.rs:934` compares `attrs ==`). It is harmless today on the app path, but it is spurious-change bait for C1b's legacy-agreement and dedupe | Sort the kept pairs by `AttrKey` in `keep_attrs` |
| U-3 | Low | `present/flow.rs` process-global `LAZY_ATTRS` (`AtomicBool`) | A test that flips it races with every other reader test in the same `gorca-core` binary (they call `focus_fields()` concurrently), which is a latent flake | Pass the mask through the Reader config (set once at construction) instead of a global, or keep the global but never flip it in tests (test `focus_fields_for(lazy)` as a pure function) |
| U-4 | Note | Every focus on a dump row now triggers a revalidation fetch (it was a full `Hit` before) | More IPC per focus move while arrowing through cached lists. The rapid-focus hold bounds it. Watch the K/L storm numbers in C5 | — |

## M22a-C2 code review (2026-10-19, branch `worktree-agent-a95d316c70d5c4694`, 67d93461)

**Verdict: MERGE AFTER FIXES.** V-2 and V-3 are needed before C5 can gate anything. V-1 must land
before M22b1. `git merge-tree` against main `85f5bdde` conflicts only in `docs/decisions.md`
(trivial).

What I ran (target dir `/var/tmp/gorca-review-target`):
- `cargo fmt --check`: OK.
- `cargo test` for `gorca-core`, `gorca-fuse`, `gorca-sem-atspi`, `gorca-doc-source-atspi`,
  `gorca-model` and `xtask`: all pass (886 core, 30 + 34 + 18 fuse, …).
- `cargo xtask deps`: OK, including the new rule 11 (`entry_unmasked` only inside `gorca-fuse`).
- Workspace clippy with `-D warnings`: clean.

What holds:
- The normaliser runs in the source crate (H-7); `SrcRecord` moved to `gorca_model::sem`.
- Fusion lands in the left-right back copy before `publish()`, so content and semantics swap
  atomically.
- Facts are sequenced on the actor timeline (`2n` / `2t + 1`).
- Untrusted child lists are capped and only counted.
- `strip-name-ordinal` is a closed action that takes no parameters, scans the name linearly and
  never empties a name.

**Inline fusion cost (measured).** `a_big_first_block_fuses_in_bounded_time` in **release**,
3 runs at load 20: 21,001 nodes in **25–32 ms (1.2–1.5 µs/node)** (the 13.5 µs/node figure was a
debug build). Incremental batches are small and covered by the B-FUSE gates. So the first block
of a 21k-node page holds the doc actor's turn about 30 ms longer. Caret, live-region and action
messages to that actor wait during that turn. Browse reads of the published mirror do not, and
the app-actor key path is unaffected.

| # | Sev | Where | Problem | Fix |
|---|---|---|---|---|
| V-1 | Medium (before M22b1) | `doc_actor/fuse.rs` `run()` → `self.ivm.run(Budget::UNLIMITED)` inside `flush()` | ADR-0022 §2.2 sets a per-batch fusion budget of 2 ms with carry-over. Unlimited fusion adds O(batch) work (about 30 ms for a 21k first block) to one actor turn. That is tolerable while presentation still reads the legacy fields, but not once M22b1 makes `NodeMeta.sem` the source of speech | Run the stage with a 2 ms budget (focus chain and visible window first). Publish unfused nodes with their previous entry (or `None`); `pending_changed`, which already exists for facts, carries the rest to the next flush, or to a self-wakeup when the batch queue is idle. Gate: first-block flush p99 ≤ translate cost + 2 ms |
| V-2 | Medium (C5 gate) | `doc_actor/agree.rs` | Disagreements are logged at `debug` and asserted only in debug builds, so the release e2e runs cannot show or fail on them. "0 disagreements on M8" is unverifiable in C5 | Log the process counters at `info` (target `gorca_agree`) once per document close and at shutdown (count by aspect, deviations separately). Make the e2e harness parse them and fail on non-deviation counts |
| V-3 | Medium (verification gap) | e2e `M8-MSGK` on Firefox | The scenario has had **no engine gate** since CHR-0 (`411f3f16`, `m8_steps.rs:1323`, "(Chromium)" only in its description), so every Firefox M8 run after CHR-0 includes it. That makes it a scenario-selection gap, not new in C2. But no Firefox baseline exists, and the failure starts with "gorca+Space: not in a web page" (doc registration or focus join), which C2's doc-actor changes could in principle affect | Gate `M8-MSGK` to Chromium (or give it Firefox expectations). Before merge, run `M8-MSGK` on Firefox for main vs branch, 3 interleaved runs each. Identical failures → pre-existing (add a backlog row); different → a C2 regression |
| V-4 | Low | `LV` p99 108 ms vs 100 at load about 12 | Probably load; V-1 adds up to the first-block cost to a doc turn | Rerun LV on a quiet host with main as the A arm |
| V-5 | Note | `gecko.toml` adds `□ ` and `– ` prefixes and the ordinal rule | Same legacy (X1/WebQuirks) behaviour, so agreement holds. A real author name starting with "– " on a list item is stripped | Accept |

## M22a-C1b code review (2026-10-19, branch `worktree-agent-a44216408ac5e4442`, 63630401)

**Verdict: MERGE AFTER FIXES. Merge order: C2 first, then rebase C1b (W-1).** The K1 outlier
has no C1b mechanism behind it (W-2), but a quiet-host rerun is still required before C5.

What I ran, on a clean `git archive` of 63630401 under `/var/tmp` (target dir
`/var/tmp/gorca-review-target`):
- `cargo fmt --check`: OK. Workspace clippy with `-D warnings`: clean.
- `cargo test` for `gorca-core`, `gorca-connector-atspi`, `gorca-mirror`, `gorca-model`,
  `gorca-sem-atspi` and `xtask`: all pass, also with `--features gorca-core/seg-origin`.
- `cargo xtask deps`: OK, including the masked-entry rule.
- **The worktree has uncommitted work-in-progress** (`app_actor/fuse.rs`, `store/mod.rs`, …) that
  **fails clippy** (`items_after_statements`, `crates/gorca-mirror/src/store/mod.rs:198`
  `const HDR`). Fix it before the next commit.

**W-1 merge order: C2 first, then rebase C1b.** Both branches merge cleanly with main
`85f5bdde` on their own. Merged with each other they produce **two textual conflicts plus a silent
compile break**:
- `crates/gorca-model/src/sem/mod.rs` auto-merges with **both** `pub mod record;` (C2) and
  `pub mod src_record;` (C1b), each re-exporting `SrcRecord`, `Notes`, `RoleSource` and `TagName`,
  so the crate does not compile. The two files differ only in comments. Keep C2's `record.rs` and
  delete `src_record.rs`.
- `xtask/src/main.rs` conflicts: C2's `entryrule` and C1b's `unmasked` implement the same rule
  (`entry_unmasked` only inside `gorca-fuse`). Keep one (C2's, which is already reviewed) and drop
  the other.
- `crates/gorca-sem-atspi/src/roles.rs` has an import conflict; it is trivial.

C2 is reviewed and already fixed (9018cde6: budgeted doc fusion, agreement gate, MSGK filter), so
it should land first and C1b should rebase onto it.

**W-2 K1 outlier (`c1b-c1b-3`, p99 349.6 ms, p99.9 369.6, max 372, lost 1).** Attribution:
**external stall, not C1b**, based on the following evidence.
- The pattern (p99 ≈ p99.9 ≈ max, about 1 % of 1,199 samples) is one stall of about 370 ms with
  queued echoes behind it.
- gorca did almost no work in that run (topology `topology.1213418.json`): all worker threads
  together used about 25 ms of CPU across 17.7 s; the RT lanes (input 134 ms, speech 195 ms,
  reader 56 ms) are in line with the other runs; there were 0 watchdog or XCPU demotions. A
  370 ms gorca-side compute stall would show as worker CPU.
- C1b's work on that path is bounded: `FuseWriter::step` has a 200 µs budget and reads the clock
  every 16 nodes. `attach_sem` uses lock-free `scc`/`ArcSwap`, takes no lock the RT reader
  needs, and calls `reserve(.., may_evict = false)`.
- The load average rose from 16.8 to 28 during the run. The fake app and the private
  dbus-daemon run at normal priority.
- 1 outlier in 8 runs vs 0 in 8 is not significant (Fisher p = 1.0).
- Condition before C5: 10 + 10 interleaved K1 runs on a quiet host (`bench/b1/quiet-host.sh`),
  with the reader log kept (the e2e run does not save it today, so this outlier cannot be traced
  further).

| # | Sev | Where | Problem | Fix |
|---|---|---|---|---|
| W-3 | Medium | `app_actor/fuse.rs` `attach(mirror, all = true)` when `pending == 0` | Not bounded by the step budget: after a large dump (`Cache.GetItems` of thousands of rows) one step attaches **every** waiting entry, cloning a `NodeSnapshot` per node on the app actor's turn. That can take milliseconds on the actor that carries focus events | Bound `attach` by the same deadline (or `AWAITING_SCAN` per step) and carry the rest; it is already resumable |
| W-4 | Medium (fix now) | Normaliser deviation "empty state set → Disabled" | When a disableable node reports an empty state set (a GTK node mid-construction, or a states reply that failed into an empty set), the fused entry says `Avail::Disabled`, and M22b1 would speak "unavailable" where legacy says nothing. That is a wrong-information bug in waiting | Fix in `gorca-sem-atspi` now: an empty state set on a disableable role leaves `avail` Unknown (or Enabled), never Disabled. Keep the witness as a regression test |
| W-5 | Accept (document) | GTK tab `PRESSED` → `Select::On` | For role `tab`, ARIA/Core-AAM says *selected*; GTK4 stack-switcher tabs signal it with PRESSED. The model is more correct than legacy | Keep it as a listed deviation with its witness; add a GTK4 quirk row in B5's table if it should be scoped to that toolkit |
| W-6 | Note (C3/M22b) | Open items: rare text (placeholder, shortcuts, valuetext, roledescription) and `name_from` have no registers; no reparent API; property events do not invalidate the engine | Acceptable while presentation reads legacy. All three must be closed before M22b1 switches speech: add registers or groups for the rare text, `Ivm::reparent` (C3 needs it for content names), and engine invalidation on property events (otherwise the entry can lag a `Stale` snapshot) |
