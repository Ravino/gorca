# M22b1 plan: `gorca-present` for change and focus (P1, P4)

Source: ADR-0022 section 2.5 (pipeline), 2.9 (extension paths), 2.11 (migration), 5 (M-6, M-9,
M-13); `docs/reports/m22a-c3-review.md` (M-3, L-1, L-4 are pre-M22b items); decisions rows of
2026-10-23/24. Owner principles: async actors, explicit state machines, message passing, no
synchronous coupling. Slice 1 is done in this change; slices 2 to 5 are planned.

## Shape

* `gorca-present` is a neutral crate. Compose, render, diff and the deviation list are pure and
  synchronous. The shadow presenter is an **actor** (`ShadowActor::run`) the Reader lane owns in a
  `JoinSet`; the Reader offers samples through a bounded channel (`try_send`, depth 64, drops
  counted). Results are counters and a `gorca_present::shadow` debug log; the actor never calls back.
* Shadow compares **token multisets** (kind, literal) of the legacy presenter's utterance and of
  `render(compose(entry))`. Order-only differences are counted apart. Allowed differences are rows of
  `crates/gorca-present/deviations.toml` (id, surface, reason, `until`); a classifier in
  `diff.rs` produces the id, a missing row means "not allowed".
* Enable with `GORCA_PRESENT_SHADOW=1` (`CoreConfig::present_shadow`). The summary is logged at
  `info` when the Reader lane ends. The default flips to on in slice 3.

## PresIr v1 (frozen at the end of slice 2)

`gorca-present-ir`, `PRES_IR_VERSION = 1`: `Content::{Text, Msg{id,args}, Ext(ns,key,args),
Opaque}`, `Field { kind, flags, lang, content, origin, span }` (56 B), `PresIr { .., flags, fields }`,
`Reason` with `Unknown(u16)`, `ExtKinds` (host-allocated add-on kinds), `PresIr::redacted`. Wire rules
(tested): unknown kinds pass through unedited; words are catalog key strings; numbers stay numbers;
add-on text is a catalog reference; secrets are redacted. Frozen after the replay table showed the
shape covers every native field. Review of the freeze: opus, before merge.

## Slice 2 result (2026-10-24)

* Native role table `gorca-present::roles` (exhaustive over `SemRole`, M-10 unmapped list) replaces
  the `legacy_family` detour; a button with a pressed state is a toggle button; native static text
  says "label".
* Range value: the connector puts a numeric AT-SPI `Value` into `value_text` when the toolkit gave no
  `valuetext`; the old `value-not-fused` row is gone. A field's contents stay text-layer data
  (`field-contents-not-fused`).
* Native corpus replay (GTK4, 375 nodes): 364 equal, 11 explained by deviation rows, **0 different**
  (was 169). Ceiling set to 0. Rows (until 2026-12-15): `field-contents-not-fused`,
  `content-derived-name`, `pressed-without-toggle`, `role-from-model`.
* Storm bench (M-3) and L-4 guard landed earlier in this slice.
* Release measurement of the shadow (R-1): `cargo test --release -p gorca-present on_the_pool`,
  20000 samples, load average 7: offer mean 281 ns, worst 29 us, 0 dropped, 20000 compared.

## Slices

| # | Content | Days | Acceptance |
|---|---|---|---|
| 1 (done) | IR types (draft v0), crate, `native.focus` rule, render, diff + `deviations.toml`, shadow actor, Reader hook (focus utterance only), `GORCA_PRESENT_SHADOW`, counters + debug log, `SemRole::legacy_family` and `Role::msg` moved to the model | 1.5 | 24 unit tests in `gorca-present`; Reader tests: shadow never changes speech, off by default; hook cost 330 ns mean (debug build, 20k calls) which is 0.3 % of the 0.1 ms M-6 budget; fmt, clippy, deps, strings clean |
| 2 (done) | **Freeze `PresIr` v1 at the end.** Close the native gaps: Range value group (retire `value-not-fused`), `Check`/`Press` for toggles, mixed words, tab/list-item roles; offline replay harness that feeds M8 fixture snapshots to `compose` and `legacy` and prints the diff table; pre-M22b items M-3 (re-fusion storm bench) and L-4 (entry staleness) | 2 | Replay: native focus, **0 differences outside `deviations.toml`** on the M8 native fixtures; `deviations.toml` has no `native.*` rows left; storm bench p99 within M-11 |
| 3 | Web focus (`present_web`): port NVDA word tables to `compose` rows (current, haspopup, level, error text, description pause, roledescription PR-5), position PR-4; `Reason::Change` (state-change, ST-1..6) shadow; default shadow **on** in the daemon | 2 | tier-1 covering array + M8 web fixtures: diff <= 0.5 % tokens, every remaining row in `deviations.toml` with reason; live shadow soak of 1 h of use: `different` ratio < 1 %, `dropped_full` = 0; P0-P10 and F1-F10 pass |
| 4 | C3b inferred form label as a **lowest-trust fused fallback**: stored in the S4 stratum with taint, the layout revision it was measured at, and the `browse.hedge_inferred` switch (default off = plain); `FieldKind::INFERRED_LABEL` rendering; policy / window lead-in / `web_extra` edits expressed as IR edits (hook points, no plugin yet) | 2 | Late Reader overlay removed; label invalidated when layout rev changes (test); hedge on speaks it hedged, off plainly (both tested); memory: fallback adds <= 16 B per labelled field, 100k-node page stays within the M-9 25 MB budget (measure with the B-FUSE bench) |
| 5 | Switch focus and change to `PresIr` render; delete legacy `present_focus`, `present_web` and the web overlay duplicates in the same slice; M-6 on the real bench | 1.5 | M-6: focus keypress to speech **Δp99 <= 0.1 ms** vs main before M22b1 (`gorca-bench` e2e focus run, interleaved on/off, GTK4 and Firefox fixtures, same load and governor recorded); M-13 zero outside deviations for P1/P4; `PresIr` hot-path allocations measured (a `SmallVec<[Field; 6]>` should not spill for focus); legacy code deleted, deviation file has no `focus` rows |

Gate per slice: `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, tests of
the touched crates, `cargo xtask deps`, `cargo xtask strings`, a `docs/decisions.md` row.

## Memory budget

No per-node storage is added by slices 1 to 3 (the IR is built per utterance and dropped). Slice 4 adds
the inferred-label fallback only for nodes that got one. The shadow queue holds at most 64 samples
(about 64 x 200 B). `Field` is asserted <= 48 B (test `record_sizes_stay_small`).

## Risks

* Shadow offers the raw `presenters.present` output, before policy, window lead-in and web extras;
  those edits are slice 4 and will show as differences if a node triggers them.
* Per-app presenters registered by extensions are compared against the generic rule; expect
  deviations for them until M22b4.
* `Role::spoken()` words are used for the role field through `SemRole::legacy_family`; roles the
  legacy set has no word for (`legacy_family == Unknown`) give no role field. Slice 2 replaces this by
  a catalog `role-*` table keyed on `SemRole`.

## Round 2 review fixes (2026-10-24)

F-1 `LangId` is a host-interned BCP-47 tag (wire = string); F-2 `ArgVal::Opaque`; F-3 the shape
test pins field types and exact sizes (`Field` 56 B, `Content` 24 B) and covers `Arg`, `ArgVal`,
`FlatSpan`, `LangId`; F-4 `ExtNs` token bound by the host plus a 256-kind quota; F-5 `role-from-model`
is gated on a role allow-list; F-6 `field-contents-not-fused` covers text entries only; F-8
`pressed-without-toggle` is tabs only; F-10 an unknown message key is `Opaque { 0xFFFF }`. Scroll
bar and tooltip now have their own catalog words (`scroll bar`, `tooltip`) instead of slider and
popover. The shadow sample is taken before the edits but sent after the speech effect (off the
latency path). Not done: F-7 switch words ("switch on/off") need a legacy `Role::Switch` and catalog
state words in both presenters; moved to slice 3. F-9: the slice 2 criterion "no `native.*` rows
left" is amended to "every remaining row is dated, gated per role, and witnessed or unit-tested";
4 `focus` rows remain (until 2026-12-15). F-11 (`Redacted` newtype) and F-12 (co-firing counts) are
slice 3.

## Owner rule (2026-10-24): no legacy tails

Shadow mode is temporary. Each surface has an **expiry date** and **exit gates**; when a surface is
cut over, its legacy path is **deleted in the same slice** and no flag keeps it alive. Pre-fusion
fallbacks (C3's `NodeMeta` fallback in the web overlay, the late Reader label overlay) move into the
model as lowest-trust provisional facts (S4 stratum, tainted, with their revision); they do not stay
as reader-side branches. An architect amends ADR-0022 and adds an `xtask` rule with a dated allowlist
of legacy uses; this plan follows it.

Revised slices 3 to 5 (each ends with deleting what it replaced):

| # | Content | Deleted at the end | Exit gates / expiry |
|---|---|---|---|
| 3a (done in this pass) | `Redacted` newtype (sinks take it, spans cleared); GTK switch words in both presenters (`Role::Switch`, AT-SPI role 130, "switch on/off"); host-only `ExtKinds`/`LangTags` by cargo feature; exact size asserts | the toggle-button-for-switch behaviour of the native row | native corpus replay stays at 0 different |
| 3b | Web focus rows in `compose` (NVDA words, current, haspopup, level, error text, description pause, PR-4/PR-5); `Reason::Change` rows (ST-1..6) | `present_web` and the `aria.rs` word tables it used, the web overlay's `NodeMeta` fallback (becomes a lowest-trust provisional fact in the fuse) | tier-1 array + M8 web fixtures: 0 outside deviations; shadow expiry 2026-12-15 for the surface |
| 4 | C3b inferred label as an S4 fallback with taint, layout revision and the `browse.hedge_inferred` switch; policy / lead-in / web-extra as IR edits | the late Reader label overlay (`web_label_arm` path), `window_lead_in` string edits | label invalidates on layout revision; both hedge settings tested |
| 5 | Cut focus and change over to `PresIr` render; M-6 on the real bench | `present_focus`, `present_with`, `present_web`, the shadow hook itself, `GORCA_PRESENT_SHADOW`, `deviations.toml` focus rows | M-6 Δp99 <= 0.1 ms interleaved GTK4 + Firefox; M-13 zero; memory within M-9 |

The shadow hook, `GORCA_PRESENT_SHADOW` and the replay ceilings are removed in slice 5; nothing about
them outlives the cut-over.
