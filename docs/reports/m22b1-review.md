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
