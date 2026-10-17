# Capital indication: adversarial review

Date: 2026-10-17. Reviewer. Branch `worktree-agent-ab3d30e3d665b908a`, head `d53e4dd3`
(one commit on top of `7393fdba`). Subject: `docs/reports/capital-indication.md`.

**Verdict: MERGE AFTER FIXES.** The pitch fix is correct and the pitch cannot stick. Two
must-fix items: the `sound` option is inert in every real setup today (wired so that it
is silently dropped on the default backend, and cancelled by speech-dispatcher's own
priority rules on `backend = "ssip"`), and an invalid `speech.caps` value turns all capital
indication off while the diagnostic says "using the default".

## Gates (run, not trusted)

`. ./env.sh`, `CARGO_BUILD_JOBS=4 RUST_TEST_THREADS=4`, in the branch's own target:

| Gate | Result |
|---|---|
| `cargo fmt --all -- --check` | rc 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | rc 0 |
| `cargo test --workspace` | rc 0, 72 `test result: ok` lines, no failure |
| `cargo xtask strings` | `ok (315 literals, 102 allowed)` |

Probes: two temporary integration tests, run and then deleted (the tree is clean again):
- `raised_pitch` over every base in -100..=100 and every delta in 1..=100: it always moves
  and stays in range. At delta 30 the smallest move is 15 (base 85: up to 100). At base 100
  the move is 30, downwards to 70.
- `load()` with a wrong `speech.caps` value: see F-2.

## Findings (ranked)

### F-1 MAJOR: `sound` never makes a sound in a real setup
1. **Default backend (`auto`, the user's config): dropped silently.**
   `crates/gorca-speech/src/failover.rs:184-193` sets fixed facade caps with
   `sound_icons: false`. `Lane::capital_fx` (`lane.rs:572-576`) ANDs the cue with
   `self.client.caps().sound_icons`, so with `FailoverClient` the cue is always removed,
   even while speech-dispatcher is the active backend. No diagnostic says so.
2. **`backend = "ssip"`: speech-dispatcher cancels the icon itself.** The SSIP client
   lowers every command with `Prio::Text` (`gorca-speech-ssip/src/lib.rs:219`
   `lower(cmd, Prio::Text, ..)`), so the `SOUND_ICON` and the `SPEAK` right after it are
   both `text` priority. When the `SPEAK` is queued, `resolve_priorities(SPD_TEXT)` runs
   `stop_priority_except_first(SPD_TEXT)`
   (`refs/speech-dispatcher/src/server/speaking.c:1009-1010, 941-965`). That call keeps only
   the newest text message and runs `stop_priority`, which calls `output_stop()` when a text
   message is playing (`speaking.c:861-869`). The icon is either dropped from the queue or
   stopped a few microseconds to milliseconds after it starts. The two lines are written in
   one pipelined batch, so the race always goes the same way. The mock server
   (`gorca-testkit/src/speech_server.rs:435`) does not model priorities, so the tests pass.
   This finding comes from reading the speech-dispatcher source; I did not run it against a
   real server (live-session rule). The icon file is present on the host:
   `/usr/share/sounds/sound-icons/capital`.
3. voicego reports `sound_icons = false` (correct: it would speak "capital").

Failure scenario: the user sets `caps = ["sound"]` and hears plain letters on every backend.
With `["sound"]` alone the letter is also marked `SegmentKind::Capital` but gets no pitch, so
it sounds exactly like `[]`.

Fix (pick one before merge):
- (a) Make it work on SSIP. Send the icon with a priority that a following `text` message
  does not cancel, and keep the letter at `text`. For example, send the cue at `message`
  priority: `message` stops older text, which is harmless because the cue goes first. This
  needs a test against a real speech-dispatcher in a private session. Also forward
  `sound_icons` through `FailoverClient::caps()` from the active backend, or make the lane
  ask per backend.
- (b) Ship it honestly. Keep the option, document in `config-reference.md` and the
  `caps` DOCS row that `sound` currently plays only on the planned cue sink (ADR-0017/M13),
  and emit a load-time warning diagnostic when `sound` is set. Track it in the backlog.

I recommend (b) now and (a) or the ADR-0017 cue sink later. Option (a) on the SSIP path
alone still leaves the default `auto` backend silent.

### F-2 MAJOR (small fix): invalid `speech.caps` turns indication OFF while it says "using the default"
`crates/gorca-config/src/load.rs:367-369` assigns `read_caps(..)` unconditionally, and
`read_caps` starts from `Caps::OFF` (`load.rs:494`). Measured with `load()`:

```
caps = 3        -> Caps{pitch:false,sound:false,word:false}  diag "...using the default"
caps = "loud"   -> OFF, same diag
caps = ["lod"]  -> OFF
caps = "Pitch"  -> OFF
```

Every other enum setting keeps the default on error (`read_enum` returns `None` and does not
assign anything). One typo on a live reload removes all capital indication for a blind user,
and the message tells them the default is in use. Fix: when the value is the wrong type, or
when no valid name was read but at least one error was reported, keep the previous/default
value (return `Option<Caps>`). An explicit `[]` or `"off"` still means off. Add the failing
case above as a test.

### F-3 MINOR: `caps_pitch_change` is not "percent of the pitch range"
The daemon passes `pitch_percent` straight through as the wire delta
(`gorca-daemon/src/lib.rs:163-166`). The wire scale is -100..100 (200 units), and the user's
`speech.pitch` 0..100 maps to `(p-50)*2`. So the default 30 is 15 % of the range, or 15 user
pitch steps. NVDA's `capPitchChange` 30 is 30 steps on its 0..100 scale. The capital pitch
is therefore half of NVDA's and of what the docs say (`settings.rs:369, 918`,
`config-reference.md`). This was already true for the old constant, but the new setting now
documents the wrong unit. Fix: scale by 2 (`min(100)*2` clamped to i8, so `raised_pitch` gets
up to 100), or document it as "wire units (1 = half a pitch step)". Doubling changes how it
sounds: confirm with the user.

### F-4 MINOR: `mark_capital` relies on the text shape to detect "word"
`quick.rs:214-219` detects whether the verbalizer already added the word by looking for the
prefix `"{cap} "` in the text. It works today: the tests cover en and ru, and neither symbol
table names an uppercase letter (scanned `symbols-en.dic`/`symbols-ru.dic`). But a future
`char_name` for an uppercase letter (for example a named "Ё") would skip every indication for
it, because `one_char` returns the name before the caps check and `is_single_capital(name)`
is false. Low risk. A flag on the utterance would be sturdier than parsing the text.

### F-5 INFO: SOUND_ICON events land in the tracker's `early` buffer
The icon's `701/702/703` events carry an id that is never bound, so they sit in `early`
(bounded by `MAX_EARLY`, oldest evicted first, `track.rs:179-182`). Nothing leaks and
real early events are newer than the stale ones, so they survive. This is only a concern
once F-1(a) makes icons actually play.

## Checked and fine

- **Pitch cannot stick (SSIP).** The raise is written before `SPEAK`. The restore is appended
  to the parked body (`lib.rs:221-227`), and no other command is encoded while a body is
  outstanding (`lib.rs:415`). If `SPEAK` gets a non-230 reply or no reply, `DATA_DEADLINE`
  marks the client Down, so the session (and its pitch) dies with the connection, and the
  reconnect re-sends the voice parameters. `Replace` is `STOP` + the same unit, so the restore
  is still the last line. A write error also brings the client Down.
- **Pitch cannot stick (voicego).** Raise, `SPEAK` and restore go out in one buffer
  (`voicego/src/lib.rs:232-241`). A failure kills the connection, and the base is per
  connection.
- **Restore uses the configured pitch, including after a live reload.** `base_pitch` is
  updated from `SetParams` at encode time, in order with the utterances, in both clients. A
  reload's `SetParams` cannot slip between a raise and its restore.
- **Latency.** No new round-trip on the key path. The raise line is pipelined before `SPEAK`,
  the restore rides with the body, and `SOUND_ICON` needs no `230`. The only wait is the
  existing `SPEAK`→`230` one, in the writer task, not in the Reader. Fast "HELLO": each
  `SPEAK` is `text` priority, so speech-dispatcher drops older queued text and nothing queues
  up. voicego is unchanged apart from `raised_pitch`. `capital_fx()` is two field reads per
  utterance.
- **Scope.** Marking happens only for single-character utterances: typed echo, char moves,
  terminal `Unit::Char`, web `Item::Char`, and `text_utterance` with a single piece of
  `How::Char` and `Spell::No`. Words and lines are not marked. Spelling gets the word only
  (phonetic and non-phonetic), with no pitch, because it is one message. Digits and symbols
  are not uppercase. Cyrillic capitals are detected through `char::is_uppercase` (test
  "заглавная B"). Caps Lock typing marks the uppercase letter it produces, as NVDA and Orca
  do. The order for word+sound+pitch is: icon, then "cap/заглавная X" at the changed pitch.
  The word comes from the catalogue (`MsgId::WordCap`). `xtask strings` passes.
- **Config.** List form, legacy `"pitch"`/`"say"`/`"off"`, unknown names inside a list
  (diagnostic, the rest still counts), `caps_pitch_change` 0..100 with an error at 101,
  the `SETTINGS` row count of 37, the print-default rows, the `config-reference.md` example,
  and the ADR-0007 table. `save()` does not touch `caps`.
- **Reader→lane sync.** `sent_capitals` starts equal to the lane's `CapitalFx::default()`
  (30, no cue) and is re-sent on change (test).

## Merge and overlap risk

- `git merge-tree main HEAD`: the only textual conflict is `docs/decisions.md`. Main has
  `9d45318d` with the decision row for the same topic, and this branch appends a second row.
  Keep both rows, or merge the branch row into main's.
- I checked the other live branches that touch the same files (unified editing
  `a12af19a…`, site extensions `aa50a607…`, m9r4 `a5bd784f…`). None of them adds
  `Caps::Pitch/Say/Off`, `Verbalized { pitch_delta: .. }` literals, `.verbalize(&u)` or
  `SpeechCaps { .. }`/`SpeechCtl::` matches, so there is no compile break after the merge.
  The unified-editing branch edits `text/mod.rs` around lines 372-388. This branch's
  `text_utterance`/`mark_capital` call is at line 491. The behavioural interaction (which
  single-char utterances reach `text_utterance`) should be rechecked when that branch lands.
- Any branch that later builds `Verbalized` or `SpeechCaps` with struct literals must add
  `cue` or `sound_icons`.

## Must-fix before merge
1. F-1: either make `sound` audible on at least one real backend, or document it as not yet
   functional with a load-time warning (recommended for now). Also forward `sound_icons`
   through the failover facade, or record why it is not forwarded.
2. F-2: a wrong `speech.caps` value must keep the default, as the diagnostic says.

Follow-ups: F-3 (unit/doc), F-4 (flag instead of text parsing), resolve `decisions.md`.
