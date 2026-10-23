# M22b1 slice 1 review (reviewer, r1, 80408b03)

Scope: `git diff main...80408b03` (gorca-present crate, Reader shadow hook, `SemRole::legacy_family`
and `Role::msg` moves, ADR-0022 section 2.5 freeze note). Code read; `cargo test -p gorca-present`
run (24 passed, 0 failed).

## Verdict: BLOCK the PresIr freeze; the rest may merge with fixes

The shadow plumbing is safe for live speech (immutable borrow of the legacy utterance, `try_send`,
default off, no `unwrap`, no IPC). The frozen record is not ready: as written, `PRES_IR_VERSION = 1`
cannot express parameterised words (M22b3 position and level), cannot carry add-on catalog text,
has no room for a field attribute without reshaping, and the "append-only variant" rule is not
backward compatible across a WIT boundary. Either fix P-1..P-5 before the freeze, or merge slice 1
with the freeze wording removed ("draft v0") and freeze in slice 2.

## Findings, ranked

### P-1 High (PresIr, change before freeze): `Content` cannot express i18n words with arguments
`crates/gorca-present/src/ir.rs` `Content::{Word(MsgId), Text(Arc<str>)}`. `Word` has no arguments.
`FieldKind::POSITION` ("2 of 5"), `LEVEL` ("heading level 3"), and M22b3's error text, current and
range values all need catalog templates with arguments (`gorca_i18n::tf`). With v1 the only way to
emit them is `Content::Text(format!(..))`, which bakes the UI language at compose time and loses the
word identity for braille, PR-1 dedupe and plugins.
Fix: add `Content::Msg { id: MsgId, args: SmallVec<[(ArgName, ArgVal); 2]> }` (numbers as numbers,
so braille and speech can format separately) in v1.

### P-2 High (PresIr, change before freeze): add-on strings have no content form
ADR-0026 plugins never write prose; they return message references (`gorca-i18n/src/ext.rs`,
`U+001E ns:key...`). `ir.rs` says add-on codes "render through `Content::Text` only", and
`render.rs` copies `Text` verbatim, so a plugin's reference would be spoken with control
characters, or the plugin is pushed into literal English text. Fix: `Content::Ext { ns, key, args }`
(or render resolves references in `Text` from an add-on `Origin.via`), decided now.

### P-3 High (PresIr, change before freeze): "append-only" is not unknown-tolerant over WIT
`Reason` and `Content` are `#[non_exhaustive]` and "append-only". That protects Rust callers only.
A component lifting a WIT `variant` with an unknown case traps, so adding `Content::Msg` or a
`Reason` later breaks every installed `present-edit` plugin unless the world is versioned. Also
`MsgId` is a generated `#[repr(u16)]` in catalog order (`gorca-i18n/build.rs:111`), so its
numeric value moves when a key is added; it is not a wire id. Fix in the freeze text: WIT carries
`reason` as `{code: u16, ...}` with a documented "unknown code: pass through unedited" rule, content
as a variant with an explicit `opaque` fallback case (host keeps the original), and words by catalog
**key string**, never by `MsgId` discriminant. Test this rule alongside `field_kind_codes_are_frozen`.

### P-4 Medium-high (PresIr, change before freeze): no extension slot on the records
`PresIr`, `Field`, `Origin` are plain `pub`-field structs without `#[non_exhaustive]`, and `Field`
is exactly at its 48 B assert (2 + 8 + 12 + 24 + padding). Any later attribute is a reshape, which the
freeze forbids. Braille (M10) and plugins need at least: per-field language (`lang`, web `lang=`,
speech voice switching and braille table choice), a `secret`/redact bit (a password VALUE must never
reach a plugin or a braille display in clear; today only the shadow log checks
`snap.role == PasswordEntry`, `flow.rs` shadow_focus), and a verbosity/importance bit for the
report flags. Fix: add `flags: u16` (bitflags, reserved bits zero) and `lang: Option<LangId>` (u16
interned) now, mark the structs `#[non_exhaustive]` with constructors, and raise the size bound
with a measured number.

### P-5 Medium (PresIr): add-on `FieldKind` codes collide
`EXT_BASE = 0x8000` gives every add-on the same range; two plugins that both pick 0x8001 merge in
PR-1 dedupe and ordering. Fix: host-allocated codes per (plugin id, field name) at load, recorded in
the registry export (`field_kind_rows`), never guest-chosen.

### D-1 Medium: `value-not-fused` hides real regressions and has no expiry
`diff.rs` allows any difference whose legacy-only tokens are all `Value` and new-only is empty. The
legacy `value_text` (`present/utterance.rs:236`) emits `Value::Text` too, i.e. a text entry's
contents and `aria-valuetext`. Failure scenario: compose stops emitting `VALUE` for a slider whose
fused `value_text` is "50 percent", or never speaks an entry's contents; both count as `allowed`,
not `different`. `surface` is parsed and ignored (a focus row allows the same id on every surface),
and `until` is neither required nor checked. Fix: classify only when the entry has no
`value_text` and the legacy token equals the range current value; key rows by (id, surface); make
`until` mandatory and fail a test once the named slice's marker exists (or use a date).

### R-1 Medium: the shadow "actor" runs on the Reader's thread
`runtime.rs` `reader_loop` runs inside `reader_lane`'s `new_current_thread` runtime; `JoinSet::spawn`
puts `ShadowActor::run` on that same thread. Compose, render, two key `Vec`s, two sorts and the debug
formatting run between Reader messages and delay the next focus. The measured 330 ns covers only the
hook, not this work. It is not parallel either (owner principle). Fix: spawn it on the shared
multi-thread runtime handle and measure hook + actor in release.

### A-1 Low: avoidable allocations on the future hot path
`compose.rs` `Content::Text(name.into())` (and value text, description) copies `&str` into a new
`Arc<str>`; the entry already holds `Arc<str>`. Use `Arc::clone`. 3 allocations per focus.

### L-1 Low: layering
The moves respect layering (gorca-model owns both enums; `deps.rs` lists gorca-present as neutral).
But `gorca-present` depends on tokio/tokio-util, so if gorca-ext-api ("no runtime") needs the IR types
it cannot depend on this crate. Put `ir.rs` in gorca-model (or a tiny `gorca-present-ir`).

### L-2 Low: wildcard match
`compose.rs` `tri_state_word` ends in `_ => None` over `Role`; CLAUDE.md asks for exhaustive matches.

## Checked and fine
No `unwrap`/`expect` outside tests; `try_send` with counted Full/Closed; cancellation via child token;
actor panic stays in its task; shadow off by default and the tests prove speech is unchanged;
`legacy_family` is exhaustive with the "place new roles here" arm; no AT-SPI types in `ir.rs`
(`NodeKey`, `GroupMask`, `MsgId` are model/i18n types).

---

# Round 2: PresIr v1 freeze + slice 2 (0e437add, 2026-10-24)

Scope: `git diff main...0e437add`. Ran `cargo test -p gorca-present-ir` (10 passed) and
`cargo test -p gorca-present` (37 passed). `cargo tree -p gorca-present-ir`: gorca-model,
gorca-i18n, smallvec, thiserror, bitflags only (no tokio). The connector replay test was not run
(out of the allowed scope); the "364 equal / 11 allowed / 0 different" figure is unverified here.

## Verdict: BLOCK the v1 freeze on three cheap contract items (F-1..F-3); slice-2 code may merge

P-1..P-5 from round 1 have landed (Msg args, Ext refs, Opaque, key strings, flags/lang, ExtKinds,
redaction, Unknown reason). What remains are things that cannot be changed after v1 without a
version bump, and they cost about an hour now.

## Findings, ranked

**F-1 (High, freeze). `LangId` means a UI catalog index, not a content language.**
`gorca-present-ir/src/lib.rs` `LangId` doc: "`Lang` index + 1"; `gorca-i18n/src/lang.rs:20`
`Lang` is `{En, Ru}`. A field's `lang` exists for page/app content language (an HTML `lang="de"`
paragraph read with a German voice, as NVDA does in Firefox). A catalog index cannot express `de`,
and the wire form is unspecified (the same build-order trap that P-2 fixed for `MsgId`). Changing
the meaning later is a semantic break of v1. Fix: define `LangId` as a host-interned BCP-47 tag
(interner beside `ExtKinds`), wire form = the tag string, unknown tag = absent; doc + one test. No
shape change.

**F-2 (Medium, freeze). `ArgVal` is a closed enum with no open arm.**
`ArgVal::{Num, Text}` has no `Unknown`/`Opaque`; adding `Float`, `Date` or a nested `Msg` later
(Fluent supports them) breaks every exhaustive match and the wire decoder. Every other kind in v1
is open; this one is not. Fix: `ArgVal::Opaque { code: u16 }` (renders as nothing, passes
through), plus a code table and a round-trip test.

**F-3 (Medium, freeze). The shape test checks field names, not types or all records.**
`shape_of_the_records_is_frozen` binds every field to `_`: `sem_rev: u32 -> u64`, `group: u8 -> u16`,
`LangId(u16) -> u32` or `Origin.via` width changes all pass. `Arg`, `ArgVal`, `FlatSpan`, `LangId`,
`FieldFlags`, `IrFlags` and the `Reason` payloads are not covered. Fix: typed bindings
(`sem_rev: _s` then `let _: u32 = _s;`), exhaustive matches over `Reason` and `ArgVal`, and
`size_of` equalities (not `<=`) for `Field` (56) and `Content` (24).

**F-4 (Medium). `ExtKinds` has no per-namespace quota and trusts the caller's namespace.**
`alloc(ns, name)` takes `ns: &str`; one add-on can allocate all 32,768 codes (`EXT_BASE..=0xFFFF`)
and every other add-on gets `Full`; nothing binds `ns` to the calling add-on, so it can claim
`telegram:unread`. Not wired yet, so not exploitable today. Fix before M18a-6: the host passes the
manifest id (never guest input), a per-namespace cap (for example 256), and a test.

**F-5 (Medium, deviation scope). `role-from-model` has no gate at all.**
`diff.rs` `explain(RoleFromModel)` fires whenever the legacy utterance has no role token and the
new one has exactly one; `DiffCtx` is not consulted. It therefore hides any new role word, including
the questionable ones the reason itself lists ("slider" for a `Scrollbar`, "popover" for a
`Tooltip`). Fix: gate on a `SemRole` allow-list in `DiffCtx` (the roles behind the 11 nodes).

**F-6 (Medium, deviation scope). `field-contents-not-fused` covers spin buttons and combo boxes.**
`field_role` = `Class::Field` = `Combobox | TextBox | SearchBox | PasswordBox | SpinButton`. Text
contents are only fetched for `TextEntry`/`PasswordEntry` (`fetch/strategy.rs:322`). A spin button
gets a numeric value (`strategy.rs:308`) that `range_value_text` now fuses; if that path regresses,
the legacy "5" goes missing on the new side and this row hides it, which is the round-1 masking
problem again. Fix: gate on `TextBox | SearchBox | PasswordBox`.

**F-7 (Low-Medium, role words). Switch says "toggle button", then "checked" or "not pressed".**
GTK4 `GtkSwitch` is `ATSPI_ROLE_SWITCH` (`refs/gtk/gtk/a11y/gtkatspiutils.c:245`) -> `SemRole::Switch`
-> `RoleToggleButton`, `Class::Toggle`. On: `check == On` speaks "checked"; off: speaks
"not pressed". The vocabulary is mixed; Orca says "switch" with "on"/"off". This matches legacy
(no regression) but is now locked in by `every_role_word_agrees_with_the_legacy_family_unless_improved`.
The same holds for Scrollbar -> "slider", Tooltip -> "popover", Meter -> "progress bar". Fix in
slice 3: add `role-switch` and `role-scroll-bar`, a `Class::Switch` using `state-on`/`state-off`,
and widen the test's allow-list. Toggle button for a `Button` with a press state and "label" for
`StaticText`/`Caption` are correct. The table is exhaustive, has no `_` arm, and `UNMAPPED` is
checked against it over the whole registry.

**F-8 (Low). `pressed-without-toggle` is not scoped to tabs.** It is gated only on
`press == NotToggle && check == NotCheckable`. A model regression that drops `Press` on a role whose
legacy word stays the same would be hidden. Fix: also require `SemRole::Tab` and a `selected` token
on the new side, as the reason claims.

**F-9 (Low). Expiry is enforced only by a unit test.** `Deviations::builtin()` keeps honouring
expired rows at run time. The test does fail after 2026-12-15 (lexical ISO compare, verified by
`expiry_compares_iso_dates`). Suggest also logging expired ids in the shadow summary. Also, the
slice-2 acceptance criterion "no `native.*` rows left" is not met as written: 4 `focus` rows remain.
The orchestrator should amend the plan row, not leave it silently.

**F-10 (Low). Open-kind aliasing.** `Reason::Unknown(2).code() == 2` decodes back as `BrowseLine`;
`Content::from_wire_msg` returns `Opaque { code: 1 }`, whose `code()` claims "msg" with no key, so a
re-encoder emits a malformed msg. Fix: never construct `Unknown`/`Opaque` with a known code
(`from_code` is the only constructor; an unknown key maps to a reserved code such as `0xFFFF`).

**F-11 (Low). Redaction is not structural.** `redacted()` is opt-in, and it keeps `span` on a secret
field, which leaks the secret's length. Fix: a `Redacted<PresIr>` newtype as the only type the
plugin/braille/log sinks accept, with the span cleared.

**F-12 (Low). `Allowed(id)` reports only the first classifier used**, so `allowed` stats
under-count rows that co-fire (for example pressed-without-toggle + role-from-model on tabs).

## Overhead (M-6, Δp99 ≤ 0.1 ms)
Acceptable for shadow mode. A mean of 281 ns is 0.3 % of the budget. The 29 µs worst case is one
sample's maximum at load 7, not a p99, so it does not threaten the budget. However:
- `shadow_focus` runs **before** the utterance is spoken (`gorca-core/src/present/flow.rs:680`),
  so it sits on the keypress-to-speech path. Moving it after the speak effect takes it off that
  path for free.
- The timed region excludes the `is_web` check and the sample construction, and the producer is a
  plain thread, not the current-thread Reader lane.
Still to measure: p99/p99.9 of the offer on the Reader lane; the e2e Δp99 interleaved on/off
(slice 5, GTK4 + Firefox); pool CPU contention from compose under a 10k/s storm with the shadow on;
`dropped_full` in the 1 h live soak.

## Checked and fine
No `unwrap`/`expect` outside tests (the only one is `unreachable!` in the shape test). No new
blocking calls; `try_send` only. `gorca-present-ir` is tokio-free; the connector depends on
`gorca-present`/`gorca-core` only as dev-dependencies. The IR has no backend types.
`range_value_text` drops a non-finite min or max the same way legacy `snapshot()` does (parity),
and a toolkit `valuetext` wins over the number. The classifiers require the whole difference to be
explained before `Allowed`. Edits are ops in ADR-0026 (`present.edit`), so `Opaque` carrying no
payload is acceptable: the host keeps the original field.
