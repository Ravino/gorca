# Capital indication: why the pitch was silent, and the independent options

Date: 2026-10-17. Author: rust-engineer.

## Root cause of the missing pitch

The path was intact end to end: `Reader::mark_capital` (typed echo, character moves,
terminal), `browse::present` (web characters) mark a single capital as `SegmentKind::Capital`;
`Verbalizer::verbalize` turns that into `Verbalized::pitch_delta = +30`; both clients lower it
to `SET self PITCH base+30` before the `SPEAK` and `SET self PITCH base` right after the body
(the server snapshots settings at `SPEAK`, so only that message is affected).

The bug was in the arithmetic. `raised_pitch(base, delta)` clamped `base + delta` to 100 and
returned `None` when the result equalled `base`. The user's `~/.config/gorca/gorca.toml` has
`pitch = 100`, which the daemon maps to wire `(100 - 50) * 2 = +100`. `100 + 30` clamps to 100,
the result equals the base, no `SET PITCH` is sent, and the capital sounds like any letter. A
unit test even asserted `raised_pitch(100, 30) == None` ("nothing to raise"). The same table
shows the config: `backend = "auto"` (voicego first, speech-dispatcher fallback), `output_module = ""`.

Both backends support the per-message trick:
- speech-dispatcher over SSIP: `SET self PITCH` before `SPEAK` is captured with the message
  (`refs/speech-dispatcher/src/server/parse.c`); there is no per-utterance pitch inside one
  message without SSML `<prosody>` (which would need `SSML_MODE on` for every capital).
- voicego: `SET self PITCH` is a session setting snapshotted when `SPEAK` is enqueued
  (`refs/voicego/internal/server/session.go` around the `PITCH` case, and the speak path); it
  has no per-utterance pitch field in the native protocol.

## Fix

`gorca_output_api::raised_pitch` (one shared copy, both clients use it): when less than half
of the delta fits above the base, the capital goes the same distance below it. Base 100 gives
70, base 90 gives 60, base 0 gives 30. A user at the top of the pitch range hears capitals
lower, which is as audible as higher.

Restore cannot stick: the raise, the `SPEAK` and the restore are lowered as one unit
(`lower()` for SSIP, one `write_all` for voicego); `Replace` is `STOP self` followed by that
same unit, so the restore is always the last line of the command. A reconnect re-sends the base
through `ResendVoiceParams`. Tests: `a_replacing_capital_restores_the_base_pitch_as_its_last_line`
(lower.rs), `a_capital_is_raised_for_its_own_message_and_the_pitch_restored` (fake SSIP server),
`a_capital_at_top_pitch_goes_below_and_the_base_is_restored` (fake voicego server).

## Options (NVDA style, independent and combinable)

```toml
[speech]
caps = ["pitch"]          # default; any of "pitch", "sound", "word"; [] = none
caps_pitch_change = 30    # percent of the pitch range, used with "pitch"
```

Examples: `caps = ["pitch", "sound"]`, `caps = ["word", "sound"]`,
`caps = ["pitch", "sound", "word"]`, `caps = []`. The old single values still read:
`"pitch"`, `"say"` (= word), `"off"` (= `[]`). Unknown names are diagnostics; the valid
names in the same list still count. Applied through the verbalizer path for single characters:
typing echo, character moves/review, spelling and say-character in text, terminal and web
characters. Not applied to words or lines.

- `word`: "cap" / "заглавная" from the message catalogue (`WordCap`), never hard-coded. It is
  added in `Verbalizer::one_char` (and when spelling with the phonetic alphabet) and in
  `mark_capital` for typing echo, without doubling when the verbalizer already did it.
- `pitch`, `sound`: the Reader only marks `SegmentKind::Capital`; the lane holds the effects
  (`SpeechControl::SetCapitals(CapitalFx { pitch_delta, cue })`, sent by the Reader when the
  settings change, `CapitalLevels` -> `CapitalFx` in the daemon). Tests cover the combinations.

## Sound (earcon)

New minimal abstraction: `Verbalized::cue: Option<Cue>` and `SpeechCaps::sound_icons`. The lane
drops the cue when the client reports `sound_icons = false`.
- SSIP: `SOUND_ICON capital` is written as its own queued message before the `SPEAK`
  (reply `225` is absorbed by a `Simple` expectation; its events carry an id nobody tracks).
  The fake server now answers `SOUND_ICON`. A sound-icon file named `capital` must exist in
  speech-dispatcher's icon directory; this is a deployment item, not verified on the live
  server (no live speech was touched).
- voicego: `SOUND_ICON` is aliased to a `SPEAK` of the icon name (`SSIP_VERBS.md`, `session.go`
  `case "CHAR", "KEY", "SOUND_ICON"`), so the cue would be spoken as the word "capital". The
  client reports `sound_icons = false` and drops it. Missing: a real voicego sound-icon verb
  (QF-4 backlog), then flip the flag. `CueSink`/`gorca-cues` (ADR-0017, M13) will replace this
  per-backend path with a shared one.

## Overlap

Touched in `crates/gorca-core/src/quick.rs` (`mark_capital` only), `text/mod.rs` (the `caps`
conversion), `browse/present.rs` (two lines). Nothing in `commands/keys.rs`.

## Review fixes (2026-10-17, after `capital-indication-verify.md`)

- F-2: an invalid `speech.caps` (wrong type, unknown name, even one bad item in a list) now
  reports a problem and keeps the default `["pitch"]`; names are case-insensitive; `[]` and
  `"off"` still mean off on purpose.
- F-1 (option b): **`sound` is not working yet.** It is accepted, the config loader emits a
  warning when it is selected, and `config-reference.md` says so. Two root causes, recorded as
  a TODO in `gorca-speech/src/failover.rs`: (1) the failover facade has fixed caps, so the lane
  drops the cue even on speech-dispatcher; (2) on SSIP the `SOUND_ICON` and the `SPEAK` that
  follows are both `text` priority and speech-dispatcher's `stop_priority_except_first` cancels
  the icon. voicego aliases `SOUND_ICON` to a `SPEAK` of the name. Planned with the cue sink
  (ADR-0017/M13).
- F-3: `caps_pitch_change` is in steps of the 0 to 100 pitch setting, as in NVDA; the daemon
  maps one step to two wire units (default 30 is +60 on the wire, clamped to 100).
- F-4: character echo (key echo, text layer, terminal) is verbalised with `char_opts`, which
  leaves the capital word out; `mark_capital` is the only place that adds it, so it no longer
  parses the text for the word.
