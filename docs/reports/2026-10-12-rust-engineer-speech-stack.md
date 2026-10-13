# Speech stack (Phase 1): notes and measurements

Scope: `gorca-output-api::track`, `gorca-speech-voicego`, `gorca-speech-ssip`, `gorca-speech`
(`FailoverClient`, `SpeechLane`), `gorca-testkit::MockSpeechServer`.

## Design points

- **Shared bookkeeping.** `gorca_output_api::track::MsgTracker` is the pure protocol core used by
  both clients: replies come back in command order, `7xx` events carry the server msg id, ids are
  bound to `UttId` on `225`, events that overtake their `225` are parked (16) and replayed, an
  unsolicited `5xx` fails the oldest unfinished message, `703` of an utterance we replaced with our
  own `STOP` is swallowed, and a `703` for an utterance written right behind a `STOP` that never
  started triggers exactly one resend. All tables are bounded (64 live ids, 16 early events).
- **Cancel + speak is one write.** `Replace` is encoded as `STOP self` + `SPEAK n` + data frame in
  one buffer and one `write_all`; queued commands are batched into the same buffer. No wait for
  `230` or `225`. (No `SET PRIORITY` is sent: `SpeechCmd::Speak` carries no priority, voicego
  ignores priority across messages, and the default is `text`.)
- **SSIP differs.** Text SSIP must not receive the body before `230`, nor any command while a
  body is outstanding. The writer pipelines `STOP`, `SET ...` and the `SPEAK` header in one
  write, parks the body, and the reader releases it on `230`. A server that never sends `230`
  fails the connection after 2 s.
- **S-3.** Both clients keep a 4-slot command queue under a std `Mutex` (never held across an
  await). When it is full a newer `Speak`/`Replace` supersedes queued speaks; otherwise `submit`
  returns `Full`. A write that takes over 50 ms marks the client `Degraded`, over 2 s `Down`.
- **Failover.** `FailoverClient` advertises `atomic_replace = true`; a backend without it lowers
  `Replace` to `STOP` + `SPEAK`. Its supervisor is one task in the caller's `TaskSet`. In-flight
  utterances of a backend that is left behind are reported `Stopped` so the speech state machine
  cannot wait forever.
- **Lane.** `SpeechHandle::submit` puts interrupts into a single slot (newer replaces older, older
  late arrivals are dropped), the rest into a 32-entry FIFO. The lane processes new intents
  before the epoch change, so "interrupt arrives together with the epoch advance" becomes one
  `Replace`, not `Stop` then `Speak`. E-2 is re-checked at send time.

## Measurements

Mock server (loopback, debug build), `crates/gorca-speech/tests/stack.rs`:
focus (epoch advance + submit) to the server receiving `SPEAK`, worst of 10: ~0.4 ms.
This includes the lane wake-up, state machine, verbalise, failover queue hop and one write.

Real `voicegod` (binary at `<repo>/.toolchain/scratch/voicego/run/bin/voicegod`, null sink,
private socket, espeak-ng; this build is **pre-fix**, see below), manual test
`crates/gorca-speech-voicego/tests/real_voicegod.rs` (`--ignored`):

| Metric | Result |
|---|---|
| plain speak, submit to `701 BEGIN` | p50 1.9 ms, max 2.7 ms (n=20) |
| replace (STOP + SPEAK pipelined) to `BEGIN` of the new utterance | p50 11.9 ms, p99 25.5 ms, max 27.7 ms (n=50) |
| utterances lost after the pipelined STOP | 0 (43 of 50 were resent once) |

The 43 of 50 resends reproduce the documented voicegod bug (`docs/platform/voicego-protocol.md`
section 6: a SPEAK pipelined behind STOP is answered `703` without `701`). The guard in the
client turns that into one extra round trip (about +10 ms) instead of lost speech. Once the
`fix/p0-stability` voicegod is deployed, replace latency should drop to the plain-speak figure.
Not measured: a voicegod with the fix.

## Open points

- voicego has no `Mark`-bearing SSML path wired (`marks` is ignored by the voicego client).
- `Pause`/`Resume` are sent but voicego only acknowledges them.
- The Reader (parent) must call `SpeechHandle::notify_epoch` after every `EpochCell::advance`
  that is not followed by a `submit` (key press, mode change).
- Failover tests use a fake backend; the end-to-end switch over real sockets is in
  `tests/stack.rs`.
