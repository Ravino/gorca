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

## PresIr draft v0 and the freeze (moved to slice 2)

The first slice-1 freeze was blocked by review (`m22b1-review.md`). The records now live in
`gorca-present-ir` (draft v0, `PRES_IR_VERSION = 0`): `Content::{Text, Msg{id,args}, Ext(ns,key,args),
Opaque}`, `Field { kind, flags, lang, content, origin, span }` (56 B), `PresIr { .., flags, fields }`,
`Reason` with `Unknown(u16)`, `ExtKinds` (host-allocated add-on kinds), `PresIr::redacted`.
Wire rules: unknown kinds pass through unedited; words are catalog key strings; numbers stay numbers;
add-on text is a catalog reference. **Freeze to version 1 at the end of slice 2**, after the replay
table shows the shape covers every native field, with a test per wire rule.

## Slices

| # | Content | Days | Acceptance |
|---|---|---|---|
| 1 (done) | IR types (draft v0), crate, `native.focus` rule, render, diff + `deviations.toml`, shadow actor, Reader hook (focus utterance only), `GORCA_PRESENT_SHADOW`, counters + debug log, `SemRole::legacy_family` and `Role::msg` moved to the model | 1.5 | 24 unit tests in `gorca-present`; Reader tests: shadow never changes speech, off by default; hook cost 330 ns mean (debug build, 20k calls) which is 0.3 % of the 0.1 ms M-6 budget; fmt, clippy, deps, strings clean |
| 2 | **Freeze `PresIr` v1 at the end.** Close the native gaps: Range value group (retire `value-not-fused`), `Check`/`Press` for toggles, mixed words, tab/list-item roles; offline replay harness that feeds M8 fixture snapshots to `compose` and `legacy` and prints the diff table; pre-M22b items M-3 (re-fusion storm bench) and L-4 (entry staleness) | 2 | Replay: native focus, **0 differences outside `deviations.toml`** on the M8 native fixtures; `deviations.toml` has no `native.*` rows left; storm bench p99 within M-11 |
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
