# ADR-0010: Localisation and the message catalog

Status: accepted (M12a, 2026-10-17). Number 0009 stays reserved for braille (Phase 3 plan).

## Context

Until M12a every spoken word was an English literal in a presenter, a command handler or an
extension, apart from a few Russian strings added ad hoc (lock keys, history, battery, punctuation
level names, "ввод"/"удалить назад", "пробел"). Phase 3 needs Russian for every message, role,
state, command description and hint, with correct plurals ("1 элемент, 2 элемента, 5 элементов"),
English as the fallback, extensions shipping their own text, and a guard that keeps literals out
of the speech paths (PR-7).

## Decisions

1. **Format: an embedded `key = template` file, not Fluent.** The Fluent crates (`fluent-bundle`,
   `unic-langid`, `intl_pluralrules`) are not in the offline registry of this project and
   `CLAUDE.md` forbids fetching packages on the fly. A catalog line is `key = template`; a
   template has `{name}` placeholders and `{n#one|few|many}` plural forms chosen by the language's
   plural rule (`Lang::plural_index`: English one/other, Russian one/few/many by the CLDR integer
   rules). `{{`/`}}` escape braces. No selectors, terms or attributes: Russian case agreement is
   solved by writing a whole message per grammatical context (for example
   `web-no-next-<category>` carries the gender and case of its noun) instead of by runtime
   inflection. If Fluent becomes available the key and argument model maps onto it one to one;
   `.po` export for translators is a mechanical conversion of the same files.
2. **`gorca-i18n`**, a pure crate (no runtime, no transport, `thiserror` only). `build.rs` reads
   `catalog/en/*.msg` and generates `enum MsgId`, one variant per key (`role-list-item` is
   `MsgId::RoleListItem`): ids are compile-time names, there is no stringly lookup, and a typo does
   not compile. Every template is parsed once, at first use of a language, into a table indexed by
   the id; formatting is a walk over pre-split pieces and appends to a caller's `String`
   (`Catalog::format_into`), with no parsing and no hashing on the path. A message the language does
   not translate falls back to the English text; the test `every_message_is_translated` keeps the
   Russian catalog complete, `every_catalog_parses_cleanly` checks syntax and that placeholders
   agree with English.
3. **Embedded, immutable catalogs.** English and Russian are compiled into the binary and live in
   `OnceLock` statics, so a message without placeholders is a `&'static str` (`t(id)`): the
   existing `fn spoken(self) -> &'static str` APIs (`Role::spoken`, ARIA words) keep their
   signatures. The plan's `ArcSwap` table is deferred to the point where a catalog can change at
   runtime (user overrides in M13/M14); the API (`catalog(lang) -> &Catalog`) does not change then.
4. **Which language.** One daemon speaks one language. The Reader publishes
   `speech.language` (`Lang::from_code`, so `ru`, `ru_RU.UTF-8`, `ru-RU`) with
   `gorca_i18n::set_language` when settings are loaded (`on_loaded`); presenters read it through
   `t`/`msg!`. Code that has the Reader's effective settings (quick commands, punctuation
   confirmation, key echo, the verbalizer) passes the language explicitly (`t_in`, `msg_in!`),
   which keeps tests deterministic. A test pins a language for its own thread with `scoped(lang)`;
   it never touches other tests.
5. **Data versus messages (IH-4).** The `@word.*` and `@level.*` rows left the symbol tables
   (`symbols-<lang>.dic` keeps symbol names and punctuation rules only); blank/object/cap, level
   names, lock keys, history, sleep and battery words are catalog messages (`words.msg`). Spelling
   alphabets are catalog messages too (`spell.msg`).
6. **Command descriptions.** `CmdSpec.help` is a `MsgId` (`cmd-<name>`); `help_text()` resolves it.
   Key names (`key-*`), echo names (`echo-*`) and modifier names (`mod-*`) are messages.
7. **Extensions** ship `i18n/<lang>.msg` under their own name (the namespace). They do not write
   prose: wherever the ABI takes a text (policy `Value`/`Say` segments, terminal `Say`, `say`,
   notices, a manifest `help = "<key>"`) they return a *message reference*
   (`gorca_ext_api::i18n::msg(ns, key, args)`, wire form `U+001E ns:key (U+001F name=value)*`).
   The core resolves it in the UI language at the points where extension text enters the
   presentation (`gorca-ext-host::convert`, policy `Value`, `BackgroundMsg::ExtSpeech`, terminal
   `Say`, the learn-mode help). A text that is not a reference passes through unchanged, so
   literals keep working (in one language) until WIT 1.0, where they get a deprecation diagnostic.
   No WIT change is needed (WIT 0.2 texts are strings). Catalogs are registered by the host from
   `<ext dir>/i18n/` (`cargo xtask build-ext` copies them next to `ext.toml`) and, for the
   first-party native builds, from the embedded copies at daemon start.
8. **Extension input is untrusted** and bounded before it is parsed or allocated: a catalog file
   over 256 KiB is refused whole, lines over about 1.1 KiB are skipped unparsed, at most 1024
   entries per extension and language and 64 extensions, keys at most 128 bytes; a reference over
   4 KiB or with more than 16 arguments is cut; arguments are never expanded again (no recursion);
   the expansion stops at 8 KiB while it runs and the result is cut at 2048 characters, and the host
   clips the resolved segment again to `MAX_SEGMENT_CHARS`/`MAX_ANSWER_CHARS`. Catalogs are parsed
   once at registration; a reference resolves by a map lookup and a walk over parsed pieces.
9. **PR-7 guard: `cargo xtask strings`** (also run by `cargo xtask deps`, so by `make deps` and
   the gate). It scans the non-test sources of the speech crates for string literals that look like
   prose (a space between letters, a capital, a non-ASCII letter) or that sit in a speech call
   (`say(`, `announce(`, `seg(`, `message(`, `Item::State(` ...) and fails on any that is not in
   `xtask/strings.allow`. The allow-list holds `path | literal` lines (log text, internal ids,
   match patterns, protocol names) and whole test-only files; its diff is the review point for a new
   literal.
10. **Dependency rule 5** (extensions depend only on `gorca-ext-api`) is read to include the pure
    leaf `gorca-i18n` that `gorca-ext-api` re-exports for message references.

## Known gaps (not M12a)

* Settings and keymap diagnostics read out by `settings.report_problems` are English (they quote
  paths and values); the settings app (M14) owns them.
* `debug.node_info` is a developer dump and stays English.
* Per-segment voice switching by `lang` (M12) is not done: UI messages are spoken in the language
  of the speech setting, with `lang = None`.
* Russian negation of a state ("не {state}") is mechanical; some combinations read oddly and are
  listed for native review.

## Consequences

Adding a message is: a line in `catalog/en/*.msg`, a line in `catalog/ru/*.msg` (CI fails
otherwise), `msg!(Id, arg = value)` in the code. English output is unchanged byte for byte except
where noted in `docs/reports/m12a-catalogue.md`.
