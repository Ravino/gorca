# M12a fix round: verification review

Branch head 7c8f8809 (fix commit 0d5539b4), merge-base 8d784101 (main). Reviewed 2026-10-17.
Host load 14-15, PSI mem some avg10 ~0.5, CARGO_BUILD_JOBS=4, RUST_TEST_THREADS=4.

## Verdict: MERGE AFTER FIXES (one small required fix, V-1)

English output is byte-identical to main on every surface probed; all gates are green; F-1..F-8
are fixed in code except that F-5 still has a two-step bypass (V-1, a one-line fix).
No `#[ignore]`, `#[allow]`/`expect(clippy::..)`, deleted tests or weakened English expectations
were added by the fix round. The test expectations it edits are Russian only and match the catalog
changes; the earlier English assertions it replaces (`"50 percent"`, `"level"` in ext-gnome-shell)
now resolve the reference and assert the same English text plus Russian.

## Gates (branch worktree)

| Gate | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace --no-fail-fast` | 2121 passed, 0 failed, 17 ignored (one run) |

b1_ic IC-12 did not recur in this run.

## English output identity (differential golden probe)

Clean copies of main (8d784101) and the branch (7c8f8809) from `git archive`, the same two probe
modules added to both (`commands/tests/zz_golden.rs`, `present/zz_golden.rs`), one shared
scratch target, run on main twice (determinism) and on the branch once. Probe copies and target
deleted afterwards.

Dumped surfaces (182,509 lines):

| File | Content | Lines |
|---|---|---|
| commands | every `CoreCmd` x repeat 1..3, without focus and with a focused checked+required check box in a window | 1,446 |
| describe | learn-mode description of every `CoreCmd` plus "not bound" | 243 |
| present | every `Role` x (no state, each `StateSet` flag, two combos) x name {Some, None, ""}: `present_focus`, `present_selected_item`, and `changed`/`present_change` on and off | 6,424 |
| time | `time_text` 12 h and 24 h, `date_text`, 24 hours x 3 minutes | 72 |
| keys | `spoken_key_name` over 0..0x3000 (unichar 0 and = keysym), 0xfd00..0xffff, 0x1000000..0x10005ff, XF86 range | 27,135 |
| speech | `Verbalizer::unit` for every char 0x20..0x2fff x Char/Word/Text x 4 punctuation levels, sample strings x 4 units, English symbol-table level names and words | 147,189 |

Result: `diff -r` main vs main = empty; main vs branch = empty. Byte-identical, including the
changed spots ("not unavailable", "not invalid entry", AM/PM at 0/4/12/17 h).
Not covered by the probe (relied on the unchanged English unit tests instead): browse/quick/review
output, caret phrases (`text/tests.rs` asserts "new line", "all selected, 5 characters",
"1500 characters selected", "one and 3 more" and is untouched by the branch), daemon notices.

## Findings (ranked)

### V-1 MEDIUM (required): F-5 is bypassable: the missing-key fallback can return a reference
`crates/gorca-i18n/src/ext.rs:275-286` (`resolve_in`). When the key is missing, the fallback
returns `key` (and `head` when there is no `:`) verbatim. An extension that sends
`"\u{1e}own-ns:\u{1e}victim-ns:secret"` passes `resolve_for(.., "own-ns")`, gets back
`"\u{1e}victim-ns:secret"`, which is itself a reference. Two runtime paths resolve again without
a namespace: `Rewrite` -> `segments_from_ext` -> `segments_text` -> `BackgroundMsg::ExtSpeech`
-> `reader.rs:721 ext::resolve`, and `ExtSink::speak/notice` (`instance.rs:289` ->
`ext.rs:164/194` -> `reader.rs:721`). Probe (run in the branch copy):
`first="\u{1e}victim-ns:secret" second="victim text"`: the extension speaks another
namespace's message. Impact is low (catalog texts are not secrets), but F-5 is reported "done".
Fix: strip the markers from both fallbacks, e.g. `Cow::Owned(sanitize(key).into_owned())` and
the same for `head`, and add the probe as a test.

### V-2 LOW: an empty terminal `Say` is spoken as "blank"
The report says "segments that resolve to empty are dropped"; that holds for `segments_from_iter`
only. In the terminal path, a foreign reference becomes `""` in `term.rs:132` (`restrict_to`),
then `terminal.rs:654` resolves it and `term_utterance` verbalises `""` as "blank"
(speech golden: `Some Char "" blank`), with `Prio::Interrupt` for `Urgency::Key`. Same behaviour
as before the late change (the early `resolve_for` also gave `""`), so not a regression.
Fix: skip `Say` whose resolved text is empty at `terminal.rs:654`.

### V-3 LOW: extension help is resolved without a namespace
`commands/handlers.rs:596` resolves an extension command's `help` with `ext::resolve`, so learn
mode can read another namespace's message. Use `resolve_for(.., ext name)`.

### V-4 LOW (doc): stale comment in the daemon
`gorca-daemon/src/lib.rs:392-393` says the first-party catalogs are "replaced by the files next to
a WASM component when the host loads one". `register_extension` now refuses a built-in namespace
(`Taken`), so a WASM ext-vte/ext-gnome-shell always uses the daemon's embedded catalog; a newer
component with new keys speaks the keys. Fix the comment; decide whether that is intended.

### V-5 INFO: builder dropped without `build()` keeps its registrations
`ExtHostBuilder.catalogs` is only unregistered by `ExtHost::drop`. If `load_dir` panics inside
the `spawn_blocking` in `wire.rs:280`, the namespaces stay registered with no extension behind
them. Harmless (they cannot be re-registered by the same daemon anyway).

## Per-finding check of the r1 items

| r1 | Status | Evidence |
|---|---|---|
| F-1 caret English | fixed | `caret.rs` uses `TextNewLine`, `TextAndMore`, `TextChars`, `TextAllSelected`, `TextSelected/Unselected`, `TextCount*`; no English literal left in `text/` |
| F-2 lint cfg(test) | fixed | python emulation of `TestSkip` over all scanned roots: every skipped span is a test module, a `#[cfg(test)]` helper fn or a `mod x;` line; nothing non-test skipped; self-tests cover fn/`mod x;`/multi-line signatures/braces in literals |
| F-3 window phrase | fixed | `info-window`, allow entry removed, en `{name} {role}`, ru `{role} {name}` |
| F-4 catalog before validation | fixed | registered after `add_wasm` succeeds, `register_extension` refuses taken namespaces, unregistered on host drop; integration test covers broken/duplicate/shadowing dirs; `load_dir` sorts paths so the test is deterministic |
| F-5 references bound to producer | partly (V-1, V-3) | `resolve_for`/`restrict_to`/`sanitize` in place at host, shell policy and core terminal boundaries |
| F-6 language plumbing | documented | `lang.rs` module docs |
| F-7 `t()` on placeholders | fixed | `debug_assert!` in `Catalog::get`, should_panic test; release falls back to English plain text or "" |
| F-8 blocking catalog reads | fixed | `O_NONBLOCK` open, `is_file`, `take(MAX+1)`; FIFO test |

## Late changes

* Wasm terminal host (`term.rs`): sanitises screen/insert text again (redundant with the core,
  harmless) and only drops foreign references; the core resolves at `terminal.rs:654`. Nothing
  else consumes the `Say` text before that point (the secret-hold and claim logic look at flags
  only), and the native and Wasm runtimes now resolve in one place. No regression found; see V-2.
* Drop empty segments (`convert.rs`): no segment kind carries meaning with empty text
  (`Text/Role/State/Value`), so dropping them only removes empty joins after the budget runs out
  or a foreign reference is silenced.
* Note: `sanitize` removes U+001E/U+001F from terminal screen and insert text before caret
  offsets are used; a screen line containing those control characters would shift the caret by
  one. Practically unreachable (VTE does not expose C0 controls in text).

## Russian wording (spot check)

The 15 fixes are applied in the catalog with matching test updates (`пунктуация: вся`, time of
day ночи/утра/дня/вечера, нажато, обязательное, неверный ввод, список выбора, не на
веб-странице, NATO alphabet corrections). Cosmetic: `spell-cyrillic` now has lower-case "цапля"
among capitalised names.
