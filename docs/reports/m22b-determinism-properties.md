# M22b determinism and equivalence properties

Status: 2026-10-24. Author: math-researcher. Scope: the cut-over to the single semantic model
(ADR-0022 section 8) must produce zero differences between old and new output except intended epoch
changes. This report states the properties, names the test that checks each, and lists every source
of nondeterminism found. No production code was changed.

Generator: `proptest` is not vendored in the project-local cargo home (`props.rs` says the same),
so all tests use the repo's seeded xorshift; the seed is in each failure message. Scale with
`GORCA_FUSE_TRIALS` (fusion, default 150) and `GORCA_PROP_TRIALS` (presenter and IR, default 1500
or 2000). All suites were also run at 10x (fusion 1500, presenter 20000): green.

Legend: **[P]** proved by argument plus tests, **[T]** tested only, **[G]** a gap (counterexample).

## Test files

| File | Properties |
|---|---|
| `crates/gorca-fuse/tests/determinism.rs` | P1: D1, D1', D2, D3, D4.1-D4.3, G-P1-a, G-P1-b |
| `crates/gorca-present/tests/determinism.rs` | P2.1-P2.8, P3.1-P3.9, G-P3-a, G-P3-b |
| `crates/gorca-present-ir/tests/wire_roundtrip.rs` | P4.1-P4.5, G-P4-a, G-P4-b, G-P4-c |

Run: `CARGO_TARGET_DIR=/var/tmp/gorca-shared-target CARGO_INCREMENTAL=0 cargo test -p gorca-fuse --test determinism -p gorca-present --test determinism -p gorca-present-ir --test wire_roundtrip`.

Tests named `known_gap_*` pin a counterexample that exists today: they pass while the gap is open
and fail when it is fixed, which is the signal to replace them by the property they document.

## P1. Fusion

Existing coverage (not duplicated): `props.rs` F1 (side-table facts, any order, duplicates), F2
(any suffix replayed), F10-F18; `ivm.rs` F7 (incremental equals scratch under random ops) and
batching/budget independence.

**D1 Arrival-order, batching and repetition independence [P, T].** Let `I` be a finite set of
writer-stamped register inputs (`Obs`, `Life`, `Inval`, `User`, `Attr`; stamps unique per
source timeline). For every sequence `s` that contains each element of `I` at least once, in any
batching and under any call budget, `drain(apply(s))` publishes the same model as the sorted
sequence.
*Argument.* Each register is last-writer-wins on its stamp, i.e. a join in a max-semilattice:
commutative, associative, idempotent. FUSE is a pure function of register state, static tables and
an epoch snapshot (Lemma 2 of `semantic-fusion-math.md`, unique result). The published model after
a full drain is FUSE of the final register state, hence a function of `I` only.
*Test.* `d1_register_inputs_are_arrival_order_batching_and_repetition_independent`: 5 variants
(whole, batches, duplicates, tiny call budget, one-by-one) x seeds. `d1_prime_...` shows the
generator is order sensitive (re-stamping in arrival order breaks the property in more than half of
the seeds), so D1 is not vacuous.
*Boundary.* The stamp must be carried by the fact (ADR-0022 section 2.2). Anything stamped on
arrival (a `BindGen` computed by the receiver, a `vis_rev` read at admission) is an order input;
PF-7 (the `(t, R, M, g)` token with `g` a function of `t`) is the rule that keeps plugin facts
inside D1.

**D2 Idempotence [P, T].** `apply(x); apply(x)` equals `apply(x)`: no entry published, nothing
pending. Follows from the no-change join (ADR-0022 section 2.2: "a no-change join stops
processing") and early cutoff. *Test.* `d2_reapplying_facts_changes_nothing` (whole stream, a
suffix, a shuffled copy with duplicates, a random subset).

**D3 Incremental equals full recompute [P, T].** After every drained batch the published model
equals `full_check()` (independent code path, no caches). *Test.* `d3_...` on shuffled streams with
duplicates; `verify()` and entry-wise equality.

**D4 Intermediate publication stream.** The cut-over gate compares what was *spoken*, which is the
sequence of publications, not only the final model.
- D4.1 `bump_global` and D4.3 call-budgeted `run`: deterministic streams (dirty heap totally
  ordered by `(class, order, node)`; aging is tick based) [T].
- D4.2 the drained state after a revocation is engine independent [T].
- **G-P1-a [G]** `touch_priority_nodes` (see findings N1): the synchronous prefix of
  `set_source_state` differs between engines when the cap bites. Counterexample: 40 visible nodes
  each with an OCR fact, `max_sync_drain = 4`; 24 fresh engines gave more than one distinct set of
  4 published nodes (`known_gap_touch_priority_nodes_stream_differs_between_engines`).
- **G-P1-b [G]** the synchronous drain is bounded by wall-clock time (N2). Zero bound fuses 1
  node, large bound fuses the cap (8): the published prefix depends on machine speed.

## P2. Presenter purity

**P2.1-P2.4 [P, T].** `render(compose(e, reason, ctx), opts)` is a pure function of
`(e.sem, e.name, e.sem_rev, reason, ctx, opts)` **and the UI language and the extension registry
(hidden inputs below)**. Tested: twice on the same value; on a deep rebuild with fresh `Arc`s (no
pointer identity); across 8 threads walking the work list in different orders; for `compose_change`
as well; the presenter reads no `prov`, `hedge`, `taint`, `known`, `stale`, `fallback` (scrambling
them changes nothing); `sem_rev` reaches only `PresIr.sem_rev`, `ReportFlags` reaches nothing.
*Proof sketch.* `compose` and `compose_change` are straight-line code over the entry with no
global read; `native_role` and the web table are `const` matches; fields are pushed in a fixed
order into a `SmallVec`; `render` is a left-to-right map. No iteration over a hash container
reaches output (checked by grep over `gorca-present/src`, `gorca-present-ir/src`: `HashMap` occurs
only inside `ExtKinds` (host feature, lookups only)).

**Hidden inputs (P2.5, P2.6) [T].** `render` is **not** a function of `(entry, reason, settings)`:
1. **UI language.** `render.rs:71,73` call `gorca_i18n::t`/`tf`, `render.rs:75` calls
   `gorca_i18n::language()`: a process-wide `AtomicU8` (`lang.rs:80`, `Relaxed`) overlaid by a
   thread-local pin (`lang.rs:82`). The IR is language free (words travel as `MsgId`); the tokens
   are not (`p2_5`). `change.rs:42` bakes `gorca_i18n::t(id)` into an `Arg::text` at *compose* time
   (language read at compose, not render). `diff.rs:291,317,329,351` read the language too, so
   the classifiers' words move with it. `set_language` during a render can yield an utterance
   whose words come from two languages (each `t()` re-reads the atomic).
2. **Extension catalog registry.** `Content::Ext` renders through a process-global
   `RwLock<HashMap>` (`ext.rs:102`): the same IR renders to nothing before `register` and to text
   after (`p2_6`). Also first-come ordering: `register` silently drops a new namespace when
   `MAX_NAMESPACES` is reached (`ext.rs:124`), so which namespaces exist depends on registration
   order.
3. Not inputs: time, locale environment, thread id, hash iteration (none reaches output).

**Fix proposal.** Add `lang: Lang` to `RenderOpts` and `ComposeCtx` (captured once per utterance by
the Reader from the effective settings), use `t_in`/`tf_in`; pass an immutable `Arc<ExtCatalogs>`
snapshot into `render` instead of the global registry. Then P2 holds as stated and a replay can
pin both.

**P2.7, P2.8 [T].** Unsupported reasons are a stable typed error; `Content::Opaque` and
unregistered field kinds render as if absent.

## P3. Shadow diff

Notation: `diff(L, N, D, ctx)` with token multisets `L`, `N`, deviation rows `D`, `ctx = (surface,
role, name_derived, untoggled)`.

- **P3.1 Stability [P, T].** The class of the outcome (Equal and OrderOnly identified) is a
  function of the multisets and `ctx`: `diff` sorts both sides before use. Repeatable; invariant
  under permutation of either side. 4x trials, three deviation lists (permissive, builtin, empty).
- **P3.2 Oracle [T].** Equal iff identical `(kind, text)` sequences; OrderOnly iff same multiset,
  different sequence; otherwise Allowed or Different (independent re-implementation).
- **P3.3 Soundness [T].** `Allowed(id)` implies `D.allows(id, surface)`; empty `D` or an unlisted
  surface never gives Allowed.
- **P3.4 Positive cases [T].** One concrete case per classifier (10) is `Allowed(id)`.
- **P3.5, P3.7 Near misses [T].** For each classifier the predicate's gates are broken one at a
  time (surface, role, context flag, token multiplicity, token kind, exact word, extra token on
  either side) and the outcome is `Different` (UNCLASSIFIED), with exact residual counts for
  unexplainable junk (`Said`, `Capital` kinds).
- **P3.6 Surface scoping [T].** The builtin rows fire on their listed surface only.
- **P3.8 End to end [T].** `ShadowActor::process` (compose, render, diff) is repeatable on
  generated samples including reversed, truncated and padded legacy tokens.
- **P3.9 Monotonicity [T].** Enabling more rows never turns an Allowed into a Different (the
  classifiers share one mutable residue in fixed order, so this was not obvious): no counterexample
  in 200 000 merged-case trials. It holds because the token gates of the ten classifiers are
  pairwise exclusive: each takes tokens of a kind/word no other classifier takes under the same
  precondition (checked pairwise by inspection of `diff.rs:258-375`).

**Gaps.**
- **G-P3-a (narrowness) [G]** `marker-stripped-name` (`diff.rs:298-312`) has no role gate, no
  context flag and no marker alphabet: it accepts any legacy Text token that merely *ends with* a
  shorter new Text token. Counterexample: legacy `"Delete all"`, new `"all"`, role Button, surface
  `focus` is `Allowed("marker-stripped-name")`
  (`known_gap_marker_stripped_name_accepts_any_word_suffix`). A dropped word in a real name is
  hidden from the gate. Fix: require the removed prefix to be non-empty, made only of marker
  characters (bullets, digits plus dot, dashes) and whitespace, or gate on a quirk-provenance flag
  like `name_derived`.
- **G-P3-b (attribution) [G]** only the first classifier that removed anything is reported
  (`diff.rs:393 used.get_or_insert`); the others that also fired are invisible to the per-row
  counters, and "which row owns a case" depends on the position in `Classifier::ALL`
  (`known_gap_only_the_first_used_classifier_is_reported`). The outcome class (Allowed vs
  Different) is unaffected. Fix: return all ids used (a small bitset) so the gate can report each
  row's real hit count and detect dead rows before removing them at cut-over.
- Note for the gate: `ContentDerivedName` takes either branch under `name_derived` only. A legacy
  name that differs from the derived one is Allowed by design (`p3_5` documents it).

## P4. PresIr

`gorca-present-ir` fixes the wire shape (codes, catalog-key strings, language tags) but ships no
byte codec. The tests carry a reference codec built only from the public API; a value the API
cannot carry would show up as a failed round trip.

- **P4.1 [T]** `decode(encode(x)) == x` for canonical IRs: all 16 flag bits, unregistered and
  extension field kinds, every `MsgId` (via its key), `i64` arguments, empty/NUL/Unicode text,
  spans, languages (as tags), 2000 seeds.
- **P4.2 [T]** the encoding is a fixed point (`encode(decode(b)) == b`) and equal values (even with
  fresh allocations) have equal bytes: one byte form per value.
- **P4.3 [T]** unknown reason code, unknown content kind, unknown argument kind, unregistered
  field kind and reserved flag bits decode without error and re-encode to identical bytes.
- **P4.4 [T, exhaustive]** all 65 527 unregistered `Reason` codes are kept (`Unknown(c)`), and the
  nine registered codes are closed under `code()/from_code()` for the payload corners.
- **P4.5 [T]** `LangId` is a table-order artifact: the same language gets another number in a
  table that interned in another order; the tag is the identity. Compare tags, never `LangId`, across
  tables. Tag normalisation is idempotent.

**Gaps (all concern values the public API lets a caller build, not values it decodes).**
- **G-P4-a [G]** `Reason::Unknown(3)` is constructible and has `QuickNav`'s code; it round-trips to
  `QuickNav`. Fix: private constructor `Reason::unknown(c)` that routes through `from_code`.
- **G-P4-b [G]** `Content::Opaque { code: 0 | 1 | 2 }` and `ArgVal::Opaque { code: 0 | 1 }` collide
  with kind codes; a strict codec cannot encode them (payload missing). Fix: same, validating
  constructors, or `NonZero` offset codes.
- **G-P4-c [G]** an unknown message *key* becomes `Opaque { UNKNOWN_KEY }`: key and arguments are
  lost, so forwarding such an IR is lossy. P4.3's "unchanged" holds for unknown codes, not unknown
  keys. Fix: `Content::UnknownMsg { key, args }`.

## Sources of nondeterminism

| # | Where | What | Reaches output? | Fix proposal |
|---|---|---|---|---|
| N1 | `crates/gorca-fuse/src/ivm.rs:1636` (also `:1616`) `touch_priority_nodes` iterates `self.prio.keys()`; `prio: HashMap<NodeIdx,(u8,u32)>` (`ivm.rs:364`, `RandomState`) | which `max_sync_drain` nodes are fused before `set_source_state` returns, and the order of the rest in `background` | intermediate publications; also the aging queue order (`sched.rs` `ages`, insertion order). Final state: no | collect, sort by `(class, order, idx)` (the value already stored) before the cap split; make `prio` an `FxMap` or `BTreeMap` |
| N2 | `ivm.rs:1665,1674,1683` sync drain time bound (`SYNC_DRAIN_TIME = 400 us`, `ivm.rs:218`); `ivm.rs:139,146` `Budget::until(Instant)` | the number of nodes fused per drain / per run is a function of machine speed | intermediate publications | replay mode: `set_sync_drain_time(Duration::MAX)` and call budgets only; keep wall clock for production |
| N3 | `gorca-present/src/render.rs:71-75`, `change.rs:42`, `diff.rs:291-351`; `gorca-i18n/src/lang.rs:80-96` | UI language is a process global read per message | tokens (hidden input, P2.5) | `lang` in `RenderOpts`/`ComposeCtx`, `t_in` |
| N4 | `gorca-i18n/src/ext.rs:102` | global extension registry; registration order decides capacity | `Content::Ext` tokens (P2.6) | immutable catalog snapshot argument |
| N5 | `gorca-present/src/shadow.rs:178-194` `try_send` into a queue of 64 | which samples are dropped depends on actor scheduling; `dropped_full` and every outcome counter are timing dependent | counters only, not speech | the zero-diff gate must call `ShadowActor::process` directly (pure, P3.8), not the queue; or block with backpressure in replay |
| N6 | `gorca-present/src/deviations.rs:113` `today_utc()` and the `expired` unit test | wall-clock date makes a unit test fail on a date | test result, not output | inject the date (`expired(today)` already takes it); keep `today_utc` out of tests |
| N7 | `gorca-present/src/shadow.rs:130-138` `different as f64 / c as f64` | float in a ratio | reporting only; exact for the counts used (< 2^53), IEEE division is deterministic | compare the integer counters in the gate, not the ratio |
| N8 | `gorca-fuse/src/side.rs:98,307-311`, `ivm.rs:284-376` many `std` `HashMap`/`HashSet` | `RandomState` iteration | checked: every iteration that reaches a `Vec` result is sorted or feeds a totally ordered heap (`release_source` sorts `anchors`, `side.rs:471`); membership-only uses are harmless. N1 is the one leak | keep; add a lint note that hash iteration must be sorted before it leaves the function |
| N9 | `gorca-present-ir/src/lib.rs:314` `LangTags::intern` numbering | `LangId` depends on interning order (P4.5) | IR equality across tables | wire and compare tags |

Not found: floating point in fusion or presentation logic, timestamps in `gorca-present`/`-ir`
logic, `thread_local!` other than the language pin, unsafe or racy shared state in the pure paths.

## Notes for the replay and diff gate

1. Freeze the hidden inputs per run: UI language, extension registry content, deviation list
   (`Deviations::parse` of a fixed text, not the embedded one if the gate must not move with the
   release), `set_sync_drain_time(Duration::MAX)`, call budgets only.
2. Compare drained models (D1-D3 guarantee order independence) and, if intermediate speech is also
   compared, avoid `set_source_state` with more than `max_sync_drain` fact-carrying priority nodes
   until N1 is fixed.
3. Treat `Different` as the only failing class; `OrderOnly` is a real difference in speech and
   should be counted separately, not as Equal (P3.2 keeps them distinct).
4. A deviation row that never fires (dead row) and a row that fires beyond its case are both
   invisible today (G-P3-a, G-P3-b); fix G-P3-b before using row hit counts as the cut-over
   checklist.
5. Observation (not determinism): the presenter ignores `known`/`stale`/`fallback`/`taint`
   (P2.3), so an `Unknown` group is presented as its default, while ADR-0022 section 2.3 says
   compose omits fields of `Unknown` groups. Intended or a slice 4 item; the gate would not see it
   if both presenters share the default.

## Risks and unverified

- No `proptest`: shrinking is absent; failures print the seed only.
- D1 covers primary register inputs at the `Ivm` level; non-primary facts are covered by `props.rs`
  F1/F2 at the side-table level. A combined shuffle of both through `Ivm::apply` is not generated.
- P2 covers `Focus` and `Change`; later rows (browse line, quick nav) do not exist yet.
- G-P1-a is demonstrated with `max_sync_drain = 4`; with the default 64 it needs more than 64
  fact-carrying priority nodes (rare in practice, certain on a large visible window with OCR/icon
  facts).
- The reference codec is a test artifact; the production codec (WIT mapping) must reuse the public
  API the same way, and should be given the same tests.
