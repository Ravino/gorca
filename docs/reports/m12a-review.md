# M12a review: localisation sweep (message catalogue)

Date: 2026-10-17. Reviewer. Branch `worktree-agent-af4b4f2f3b1b7835e` at `ef01337a` (merge-base = main `8d784101`).
Review worktree: `.claude/worktrees/m12a-review` (detached, uncommitted; target deleted at the end).

## Verdict: MERGE AFTER FIXES

The catalogue design is sound, fast and well bounded, and English output is byte-identical on every
path I could drive. Two things block: the Russian sweep is incomplete on the text-editing paths
(F-1: typing Enter and selection changes still speak English), and the PR-7 lint that should have
caught this has a blind spot that hides about 3,000 lines of non-test code (F-2). Both fixes are
small.

## Gates run (host load 8-13, governor performance, PSI mem some avg10 ~0.2)

| Gate | Result |
|---|---|
| `cargo fmt --check` | clean |
| `cargo clippy --workspace --all-targets -D warnings` | clean |
| `cargo xtask strings` / `cargo xtask deps` | ok (313 literals, 102 allowed) / ok |
| `cargo test --workspace` | all green (no `failed` result line) |
| e2e strict K2/K3/R1/A1, 1 repeat, branch | all PASS: K2 p99 0.233 ms, K3 1.029, R1 0.363, A1 5.562 (a1w 3.573) - `bench/results/e2e/m12a-review-core/20261017T105321Z.txt` |
| same on main `8d784101` (A/B) | K2 0.206, K3 1.094, R1 0.306, A1 5.397 (a1w 3.604) - run in a deleted main worktree, numbers copied here |
| first branch run `20261017T105047Z` | **FAILED, kept**: misconfigured by me (no `--impersonate-orca` in `--reader daemon:path,args`). K2/K3/R1 "no SetKeyGrabs"; A1 passed 5.869 ms |

Microbench (release, `gorca-i18n`, 2M iterations; probe deleted after the run): `t()` is 0.8 ns (en) and 1.7 ns (ru).
`msg!` with a plural is 33 ns (en) and 62 ns (ru), against 16.5 ns for the equivalent `format!`.
`ext::resolve` costs 6.5 ns on a literal and 28 ns on a reference. The hot path is not affected: K2 and A1 sit within single-run noise of main.

## (1) English byte identity: confirmed

* Differential golden 1 covers key names. `spoken_key_name(ks, 0|ks)` was dumped for 13,055 keysyms (0..0x3000, 0xfe00..0xffff, XF86) in both trees. The two dumps are identical (26,110 lines).
* Differential golden 2 covers commands. A core test drives every `CoreCmd` x3 repeats with 10 focus role/state combinations (info.* only for the extra ones), the learn-mode pass, and three settings loads with problems. Both trees produce 1,339 lines, 438 of them non-empty, and the outputs are identical. The coverage includes confirmations, ring values, punctuation, verbosity, toggles, where-am-i state words, time and date, settings-problem plurals and "command not available".
* A static check compared 838 English literals removed in non-test code against the en catalogue. The 59 without a direct match are ids, match patterns or compositions. I checked the compositions by hand: list/table plurals, landmark kinds, `no {w} {name}` became per-category messages, `, {at} of {of}`, and `settings reloaded{tail}`. All compose to the old text.
* `Role::spoken` matches main for every role (static map compare).
* **One undocumented English change (L-4):** phonetic spelling with caps=Say. On main, a non-ASCII upper-case letter (`É`, `Ж`) was spoken as "cap cap É": `spell()` added cap and so did `one_char`. The branch says "cap É" (`verbalize.rs:218`). It is a fix, but it is not listed in the report's change table.

Probes: `/tmp/.../scratchpad/probes/zz_golden.rs` (core, add `mod zz_golden;` after `mod quick;` in `commands/tests.rs`) and `zz_golden_dump.rs` (input-api). Both files also have to be copied into the repo before a rerun.

## Findings (ranked)

### F-1 HIGH: text-editing speech is still English under `speech.language = ru`
`crates/gorca-core/src/text/caret.rs`:
* `:1773` `Piece::phrase("new line")`. Typing Enter in any text field says "new line" (K2-class path).
* `:1941` `"and {more} more"` (paste/insert flood).
* `:2165-2170` `count_phrase`: "1 character" / "{n} characters". It duplicates `text-chars-selected` and has no Russian plural.
* `:2197` `"all selected, {}"`; `:2199/2206` `"selected"` / `"unselected"` (selection echo SL-3).

All of these are hit on every Russian user's typing and selection. **Fix:** catalogue them as `text-new-line`,
`text-and-more = and {n} more` (ru `и ещё {n}`), `text-chars = {n} character{n#|s}` (ru `{n} {n#символ|символа|символов}`), `text-all-selected = all selected, {count}`, `text-selected`/`text-unselected`.
English stays identical.

### F-2 HIGH: the PR-7 lint stops scanning a file at the first `#[cfg(test)]` *anywhere*
`xtask/src/strings.rs` `scan_file` `break`s on the first line starting with `#[cfg(test)]`, including one on a
test-only helper method or a `mod tests;` declaration in the middle of a file. Unscanned non-test code:
`text/caret.rs` 1,802 lines (from `:459`), `doc_actor/translate.rs` 748, `quick.rs` 336, `text/native.rs` 224, `present/fair.rs` 119,
`app_actor.rs` ~1,050 (from `mod tests;` at `:41`), plus small tails elsewhere.

With the break limited to a top-level `#[cfg(test)]` followed by `mod x {`, the lint flags the four caret.rs literals of F-1.
The single words `"selected"` and `"unselected"` would still slip through: they are passed through a variable, and `Piece::phrase(` is not in `SPEECH_CALLS`.
Probe patch: `/tmp/.../scratchpad/lint-cfgtest-probe.patch`.

**Fix:** skip only `#[cfg(test)]`-gated items (track the brace depth of the following item, or require `mod … {`). Add `Piece::phrase(`, `Piece::unit(` and `TermAction::Say` to `SPEECH_CALLS`. Add a self-test on a fixture with a mid-file `#[cfg(test)] fn`.

The in-flight unified-editing branch adds `format!("{n} spaces")` and `format!("{n} tabs")` at `caret.rs:1996/2001`. They sit in the same blind spot, so the lint will not catch them after merge either.

### F-3 MEDIUM: the "{n} {role}" window phrase is allow-listed although it is speech
`xtask/strings.allow: crates/gorca-core/src/commands/info.rs | {n} {role}` is the window part of where-am-i (`info.rs:148`).
The word order is hard-coded, so a translation cannot reorder it. **Fix:** add a catalogue message `info-window = {name} {role}` and drop the allow line.
The rest of the allow-list (102 entries) is justified: logs, protocol names, match patterns, and the documented M14 diagnostics gap.
One exception: `config_task.rs | no problems` is read by `settings.report_problems`, which is the stated M14 gap.

### F-4 MEDIUM: an extension catalogue is registered before the extension is validated
`gorca-ext-host/src/host.rs:162` calls `load_catalogs()` right after the manifest parses. That happens before the backend check (`Native` is rejected), the size check and the component compile, and also when the component then fails.
A directory whose `ext.toml` says `name = "ext-gnome-shell"` or `"ext-vte"` therefore replaces the first-party catalogue that the daemon registered from embedded copies (`gorca-daemon/src/lib.rs:394`). This happens even though the extension never loads, and even when another extension of that name is already loaded ("already loaded" is checked later, in `add_wasm`).
Effect: it can rewrite the Shell OSD percent and terminal "blank" text, capped at 2 KiB. No core keys are reachable: core messages are `MsgId`, not namespaced.

**Fix:** register catalogues only after `add_wasm` succeeds, refuse a namespace that is already owned, and call `ext::unregister` on unload or disable. `unregister` exists but has no caller.

### F-5 LOW: references are not bound to their producer
`ext::resolve` accepts any `ns:key`, so extension A can speak B's messages. Untrusted app text that reaches a resolve point is also parsed as a reference when it starts with U+001E: ext-gnome-shell passes the accessible `name` as a segment, and ext-vte passes screen lines.
A name that starts with U+001E and is over 4 KiB is silenced (resolves to ""); a shorter one can impersonate another extension's message.
**Fix:** make the host pass the calling extension's namespace and reject a foreign `ns`, and strip U+001E/U+001F from app-sourced text before it is put into segments.

### F-6 LOW: language plumbing
* `set_language` is called only in `on_loaded`. Code that holds the effective settings (`t_in`) and pure presenters (`t()`) can disagree whenever `cmd.file` changes without a load. Production always goes through `on_loaded`, and tests mutate `cmd.file` directly, which is why `on_loaded` is the only caller.
* `CURRENT` is process-global. Today no test calls `on_loaded` with `ru` (checked), but the first test that does will flip parallel tests. Suggest a debug assertion or a `#[cfg(test)]` guard.
* The locale default comes from `config_task::fill_language` (verified). The English fallback for missing keys is verified (`Catalog::build`, test `every_message_is_translated`).
* Anything spoken before the first `Loaded` is English. That is acceptable.

### F-7 LOW: `t()` on a template with placeholders returns "" silently
`Catalog::get` does this. Nothing ties `msg!(Id)` (no args) to a plain template. **Fix:** in `build.rs`, generate a `has_args` flag and add a test asserting that every `t()`-only id is plain, or have `get()` debug_assert.

### F-8 LOW: catalogue file reads can block
`host.rs:749`: `metadata` then `read_to_string` on `i18n/<lang>.msg`. A FIFO hangs startup; the manifest and module reads have the same pre-existing pattern. **Fix:** open with `O_NONBLOCK`/check `is_file()`, and read with `take(MAX+1)`.

### Merge
* Against main: no conflicts (main has not moved since the merge-base).
* Against unified-editing (`worktree-agent-a12af19ad0bb6b976`, 2 commits): textual conflicts in `Cargo.lock`, `crates/gorca-ext-api/Cargo.toml` (both add a dependency: `gorca-i18n` and `gorca-text`) and `xtask/src/deps.rs` (both reword rule 5). There are also auto-merged files that need a careful look: `text/mod.rs`, `ext-api/src/lib.rs` and `ext-vte/src/session.rs`, where `say(.., "blank")` is a message reference now.
* Semantic conflict: unified-editing adds English literals `{n} spaces` / `{n} tabs` to `caret.rs`. They are hidden from the lint by F-2.

## Russian grammar

Plurals: `plural_index` is correct for 0/1/2/4/5/11/12/14/21/22/25/101/111/112 (`template.rs` test, rerun green).
The list and table messages use the genitive after "из" correctly ("список из 1 элемента / 5 элементов").

The worst 15 strings, with suggested fixes:

| # | key | now | suggested |
|---|---|---|---|
| 1 | `level-none` + `word-confirm` | "пунктуация без пунктуации" | `word-confirm = пунктуация: {level}`; `level-none = нет` |
| 2 | `level-some` | "немного" | "некоторая" (ru NVDA: "некоторые") |
| 3 | `level-most` | "большинство" | "большая часть" / "основная" |
| 4 | `state-pressed` / `-not-` / `-half-` | "нажата / не нажата / частично нажата" (feminine, the other states are neuter) | "нажато / не нажато / частично нажато" |
| 5 | `state-not` + DISABLED | "не недоступно" | a dedicated `state-available = доступно` (en keeps "not unavailable") |
| 6 | `state-not` + INVALID | "не недопустимое значение" | dedicated "значение допустимо" |
| 7 | `state-required` | "обязательно", which reads as "necessarily" in "обязательно недопустимое значение" | "обязательное" |
| 8 | `state-invalid-entry` | "недопустимое значение" | "неверный ввод" (NVDA ru) |
| 9 | `time-pm` / `time-am` | "3:07 вечера" at 15:07; "утра" at 0-4 h | split: "ночи" 0-3, "утра" 4-11, "дня" 12-16, "вечера" 17-23, or force 24 h for ru |
| 10 | `spell-latin` | "ноябрь" (a translated November), "эко", "кебек", "отель" | "новембер", "эхо", "квебек", "хотел" |
| 11 | `spell-cyrillic` | "Ы" (letter itself), "Цецилия" | "еры", "цапля" |
| 12 | `verbosity-confirm` + values | "подробность: подробная" | "уровень подробности: подробный" (краткий/обычный/подробный) |
| 13 | `settings-reloaded-problems` | "Нажмите gorca, control, shift, R" (English modifier names, while `mod-*` are "горка/контрол/шифт") | compose from `mod-*`, or say "клавиша gorca, контрол, шифт, R" |
| 14 | `web-not-in-page` | "не веб-страница" | "не на веб-странице" |
| 15 | `history-oldest` / `-newest` | "самое старое / самое новое" | "самая старая запись / самая новая запись" |

Also noted:
* `key-escape` / `echo-escape` "выход" was already on main; NVDA ru says "escape".
* `key-insert` "вставка" and `input-key-insert` "Insert" are inconsistent, and so are `key-caps-lock` "кэпс лок" and `lock-caps` "Caps Lock".
* `web-list-box` and `web-list` are both "список", so "нет следующего списка" is ambiguous; use "список выбора".
* `mod-gorca` "горка" (a common noun, "slide") is acceptable phonetically, but brand text should stay "gorca".
* `state-has-popup` + "развёрнуто" run together: "развёрнуто есть всплывающее меню".

## Fix round (2026-10-17)

| Finding | Status |
|---|---|
| F-1 caret.rs strings | done: `text-new-line`, `text-and-more`, `text-chars` (ru plurals), `text-all-selected`, `text-selected`/`-unselected`, count variants |
| F-2 lint scans by item | done: only `#[cfg(test)]` items are skipped; `Piece::phrase(`, `Piece::unit(`, `TermAction::Say` are speech calls; fixture self-tests; 315 literals / 102 allowed, nothing new flagged |
| F-3 `{n} {role}` | done: `info-window`, allowlist entry dropped |
| F-4 extension catalogues | done: registered after `add_wasm` succeeds; a taken namespace is refused (`register_extension`); unregistered when the host drops; duplicate dirs already fail in `add_wasm` |
| F-5 | done: `resolve_for`/`restrict_to` bind a reference to its extension; U+001E/U+001F stripped from app text (`sanitize`) at the core, host and shell-policy boundaries |
| F-6 | documented: language stays process-global (one Reader per daemon) |
| F-7 | done: `get()` on a template with placeholders panics in debug and tests |
| F-8 | done: `read_catalog_file` opens O_NONBLOCK, regular files only, `take(MAX+1)` |
| 15 Russian fixes | done, including the time-of-day words (ночи/утра/дня/вечера); English keeps AM/PM |

Found while fixing: the Wasm terminal host resolved references early, so its action lengths differed from the other runtimes (term_parity); the host now only drops foreign references and the core resolves. Segments that resolve to empty are dropped.

Gates: fmt, clippy -D warnings, `cargo xtask deps` and `strings` clean. `cargo test --workspace` (RUST_TEST_THREADS=4) ran twice after the fixes: the second was fully green. A first run saw `b1_ic` IC-12 "VACUOUS ... probe says Miss" (a cache probe on the real daemon, timing under load); it passed on a solo rerun and on the second full run. It is flaky and not caused by this change. It is reported here, not hidden.
E2E strict K2/K3/R1/A1, 1 repeat, `bench/results/e2e/m12a-fix/`: K2 p99 0.253 ms, K3 1.083, R1 0.336, A1 5.849; all PASS (main: 0.206 / 1.094 / 0.306 / 5.397).
English identity: the differential golden probes of the review are no longer on disk and were not rerun. The English unit tests (time AM/PM, `text-chars`, `state-available` = "not unavailable") and the e2e transcripts pass.

## Verification fixes (V-1..V-4)
V-1: both missing-key fallbacks in `resolve_in` now `sanitize` (test `the_fallbacks_never_return_a_reference`). V-2: a reference that resolves to nothing is not spoken in the terminal path (an empty literal Say is unchanged). V-3: extension help uses `resolve_for(.., ext)`. V-4: daemon comment corrected: first-party catalogs always come from the embedded copies. Gates: fmt, clippy, strings, `cargo test --workspace` (4 threads) all green, b1_ic passed this time.
