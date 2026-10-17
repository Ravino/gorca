# Review mode left the typing echo broken (the "degraded state")

## Symptom (user, live)
After flat review on the numpad, in any text field: space is spoken as English "space", "!" and other
symbols are silent, Backspace does not speak the deleted character, letters still echo. Alt+Tab
restores it; flat review breaks it again. A second bug: numpad character reading named letters but
not punctuation.

## Root cause
`Reader::text_active()` is `Mode::Focus | Mode::PassThrough`. Review sets `Mode::Review`, and
nothing ended it except a focus change (`review_focus_changed`), a gone surface or an explicit
exit command. So after review, with the field still focused:

1. `text_key` returned false and the text layer dropped caret/edit/selection events (`text_active`
   guards every arm of `on_text`), so it spoke neither typed characters nor erasures.
2. The generic key echo (`commands/keys.rs`) took over: letters echo as the raw character (looks
   normal), "space" came from the hard-coded English `key_echo_text`, "backspace" is an action name
   and `echo.command_keys=false` silences it, and symbols were sent as `Said` text (never
   symbol-processed) to a server running `PUNCTUATION none`, so they were silent.
3. Alt+Tab produced a focus event, which ended review (`focus_follows`) and re-tracked the field.

This is the same signature as the earlier terminal/web "degraded" reports; those fixes
(`focus_audit_on_key`) do not cover it because the focus never moved. The audit also bails out when
`!text_active()`.

## Fix
- `Reader::review_left_by_key` (commands/keys.rs): a pressed key classified `PassToApp` while in
  `Mode::Review` sends `ExitReview` (cancels the pending review op, restores Focus or the Browse
  origin) and `text_resync_after_review` invalidates the tracked window (events were dropped during
  review). Lone modifier keys (keysyms 0xffe1..0xffee) do not leave review. Review commands bound
  to numpad keys are `Command` class and are unaffected.
- Generic key echo goes through `key_echo_spoken` (present/echo.rs): typed character and "space"
  via `Verbalizer::unit(.., How::Char)` (language, symbol names, caps setting), action key names
  translated for ru (`ввод`, `удалить назад`, `табуляция`, `выход`, `удалить`, `эф N`). Names still
  need `echo.command_keys`.
- Review reading (`review/commands.rs::review_voice`): characters are always named (How::Char),
  words/lines use the punctuation level (How::Word/Line), repeat-2/3 spelling uses
  `Verbalizer::spell` (letters/phonetic), object-name and status-bar spelling too. The English-only
  `char_text`/`spell_letters`/`Spell` of M4a are removed. Spelling is now comma separated like the
  text layer ("A, p, p, l, y").
- Behaviour change worth knowing: review lines/words now obey the punctuation level, e.g. a terminal
  prompt `me@box:~$ ls -l` reads "me at box: $ ls -l" at level most.

## Tests
- `text::tests::typing_after_review_echoes_like_before_in_{russian,english}`: type, enter review,
  type space and "!", Backspace; asserts review ended, "пробел"/"space", symbol named, erasure
  spoken. Verified to FAIL with the fix disabled ("typing leaves review").
- `text::tests::lone_modifiers_do_not_end_review_and_generic_echo_is_localised`.
- `review::tests::character_reading_names_every_character_in_the_speech_language`.
- Updated 3 review tests for verbalizer spelling/levels.

## Gates
fmt, clippy -D warnings, cargo test --workspace (all green), cargo xtask deps: ok. e2e (release, --impersonate-orca,
3 rounds each, host load 17-25 from other agents; logs /var/tmp/rbe/out, results bench/results/e2e/rbe-*):
- R1/R1s/RS: pass (one earlier R1 p99 3.4 ms vs 3 under load 18, passed on rerun).
- M3 typing C*/E*: only the known load-dependent E2 `e2_missing` (1 event in one round); C3 failed
  deterministically because its expectation predates the symbol tables ("?" is now spoken "question"):
  harness expectation updated.
- Strict K2/K2s/K3/K4 (GORCA_M1=1 GORCA_M1_STRICT=1): pass.
- M6 real VTE VR/VV: expectations updated ("$" prompt is no longer "dollar" at level most; review
  current line reads "me at box: $"); then pass. One VV round showed `Down: empty line heard []` and
  `BackSpace ... wanted space heard c` once, not reproduced in the final run.

## Not done
No GTK-entry end-to-end scenario for review-then-type exists yet; the unit rig drives `Reader`
exactly as the lanes do. A live check by the user is the final proof.
