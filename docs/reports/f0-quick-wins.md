# F0 quick wins

Plan: `docs/plan/phase-3-features.md` section 7.2, 7.4, 8 (rows 1, 2, 3, 5, 12).

## What was built

| Feature | Where | Behaviour |
|---|---|---|
| Lock keys | `gorca-core/src/quick.rs` (`observe_locks`), called first in `on_key` | Caps/Num/Scroll Lock press that **reaches the application** (`PassToApp` or the `ModifierPassThrough` double tap) is announced "Caps Lock on/off". New state = inverse of the Caps/Num bit in the key event's modifier state (Mutter reports the state before the key). A swallowed single Caps tap (gorca modifier) is silent. Scroll Lock has no xkb bit on GNOME: it is counted from its own presses (starts "off"). Words come from `@word.lock.*` in `symbols-{en,ru}.dic`. |
| Capital pitch | `SegmentKind::Capital` (model), `Verbalized::pitch_delta` (output-api), SSIP `lower()`, voicego `encode()` | A capital spoken alone (typed echo, caret char move, erasure, terminal char, browse char) is marked; `Verbalizer` sets `pitch_delta = +30` (NVDA `capPitchChange`). SSIP and voicego both send `SET self PITCH base+30`, `SPEAK`+body, `SET self PITCH base` in one pipelined write (voicego snapshots session settings per SPEAK, `session.go` speak path). `say` mode puts "cap"/"заглавная" before the letter, `off` nothing. Shift+letter and Caps-Lock letters both count (decided by the character, `Lu`). |
| Speech history | `quick.rs` `SpeechHistory`, `after_message` | 100 entries, memory only (Drop clears), reviewed with `speech.history_prev/next`; first "prev" speaks the newest, edges say "oldest"/"newest". Not recorded: key echo, typing echo/erasure, command-chord echo of the history commands, say-all chunks. Secrets (sensitive utterance, password field in hand, terminal secret hold) are stored as the placeholder "password"/"пароль". |
| Battery | `DesktopEffect::Battery` -> `BackgroundMsg::Battery`; `gorca-daemon/src/power.rs` | UPower `DisplayDevice` on the system bus (explicit socket address, env rule 8), each call under 200 ms inside a 250 ms deadline, no connection kept. Spoken "82 percent, discharging, 3 h 5 min remaining" (Russian too); stale answers (a key pressed since) are dropped. |
| Date/time | existing `info.time_date` | present, unchanged. |
| Sleep mode | `quick.rs` `sleep_toggle`, `after_message`, guard in `run_command` | Toggle per `AppId`, session only. While the focused app sleeps: speech effects are dropped (except the toggle's own answer), commands other than the toggle and `gorca.quit` do not run. |

Keys (all four presets): `gorca+shift+x` history prev, `gorca+ctrl+shift+x` history next; orca presets also get `gorca+shift+b` battery. NVDA presets already bound battery, sleep, copy_last.

## Not done / caveats

- `speech.copy_last` stays planned: gorca has no clipboard layer yet (M15). It must use `SpeechHistory` (already filtered) when added.
- `speech.repeat_last` (`last_spoken`) was not changed; it already has `last_spoken_is_sensitive` and skips key and command echo.
- Scroll Lock state is predicted, never corrected (no ShellHelper signal, LK-3).
- Lock-state inversion assumes Mutter's key event carries the pre-press modifier state; verified only by unit tests, not on a live session (live-session rules).
- Capital pitch for the review cursor character (`review.cur_char`) goes through the text speak path and is covered where it uses `How::Char` pieces; not separately tested.
- The pitch raise for a capital is a fixed +30; no setting yet (`speech.caps` set migration of the plan is not done).

## Security note (coordinator review)

History never stores plaintext of secrets: tests `history_never_keeps_secrets_key_echo_or_typing_echo`. History and utterances never reach logs (`Utterance`'s Debug prints lengths only). Battery, date and sleep log nothing sensitive.

## Tests

Unit: `commands/tests/quick.rs` (11), `quick`-related in `gorca-speech` symbols, `gorca-speech-ssip` lower + mock-server test, `gorca-speech-voicego` framing test, `gorca-daemon` power state map. Gates: fmt, clippy -D warnings, `cargo test --workspace`, `cargo xtask deps`: all pass.

## e2e (strict, `--impersonate-orca`, 3 repeats, voicego, host load 11-16)

`bench/results/e2e/f0-quick-wins/20261017T060425Z.{json,txt}`, reader built from 20e0ffe (`GORCA_M1=1 GORCA_M1_STRICT=1 GORCA_M4=1`, K2,K3,K4,R1): all gates PASS. K2 p99 0.245 ms (gate 5), K3 p99 1.10 ms (gate 15), R1 p99 0.352 ms (gate 3), K4 10000 switches: 0 lost, 0 doubled, 0 leaked, 0 forged.
