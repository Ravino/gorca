# voicego client wire protocol (for the gorca Rust client)

Source of truth: `refs/voicego` v2.0.4 (commit 3a0f1f2), verified against the code
and by running a built `voicegod` on a private socket with the null sink
(2026-10-12). Cite code as `refs/voicego/<path>:<line>`.

**Key fact.** voicego has no separate native protocol. The "binary protocol" is
SSIP with length-prefixed framing (`BINARY_SSIP.md`, `internal/ssip/framing.go`).
The verbs, reply bytes, event bytes and semantics are identical to text SSIP.
Measured latency is identical too. The old opcode-based `internal/binproto`
described in `AUDIT.md` no longer exists. gRPC is used only between `voicegod`
and its engine plugins and is not reachable by clients.

## 1. Transports

| Transport | Config | Notes |
|---|---|---|
| UNIX stream socket | `[transports.unix] path` (example `$XDG_RUNTIME_DIR/voicego.sock`) | default, idle timeout infinite, no auth beyond file permissions |
| TCP | `[transports.tcp] host/port`, default plaintext only from loopback (`plain_allowed`) | idle timeout 1 h |
| mTLS (unix+tls, tcp+tls) | `[mtls]` | TLS 1.3, client cert required |

Limits: 1000 connections per transport, 10 per client identity (`@unix` for all UNIX
peers, so at most 10 UNIX connections in total). A connection is either text or
binary for its whole life.

gorca should use the UNIX socket with binary framing. Open one connection for speech
and, if needed, a second one for control. Events arrive on the same connection that
issued SPEAK.

## 2. Framing

### 2.1 Text (default, speech-dispatcher compatible)

- Command: `VERB args...\r\n`. The verb is case-insensitive. Args are split on
  whitespace (`strings.Fields`). `SET` re-joins `args[2:]` with single spaces, so
  multi-word values are collapsed to single spaces.
- `SPEAK`: send `SPEAK\r\n`, wait for `230 OK RECEIVING DATA\r\n`, then send the body
  followed by `\r\n.\r\n`. A body line that starts with `.` is dot-stuffed (one
  leading `.` is stripped). Lines are joined with `\n`. A single line must not
  exceed the reader buffer (`max(64 KiB, max_frame_size)`, 1 MiB by default).
  Exceeding it closes the connection without a reply.
- Reply: `NNN text\r\n` (final line) or `NNN-text\r\n` continuation lines followed
  by `NNN text\r\n`. A response is complete at the first line whose 4th byte is `' '`.

### 2.2 Binary framing (opt-in, `BINARY_SSIP.md`)

1. Client sends the 4-byte prefix once: `AA 56 47 01` (`0xAA 'V' 'G' version=1`).
   There is no server acknowledgement. Version `0x00` or an unknown version makes the
   server close the connection. Detection is by peeking 3 bytes
   (`DetectBinary`, `framing.go:94`). Anything else is text.
2. Every message in both directions is `u32 big-endian length | payload`.
   Maximum frame is `max_frame_size` (default 1 MiB, ceiling 16 MiB). Exceeding it
   closes the connection (no resync).
3. Client to server, command frame: the verb line as in text mode, without CRLF.
   A zero-length frame is an empty command (`ERR NO COMMAND`).
4. `SPEAK <nbytes>`: send the frame `SPEAK 5`, receive the frame
   `230 OK RECEIVING DATA\r\n`, send exactly one data frame with the raw UTF-8 body
   (no `.` terminator, no dot-stuffing). The data-frame length is authoritative.
   If `<nbytes>` was given and differs, the server closes the connection. Bare
   `SPEAK` is also accepted.
5. Server to client: one frame per logical response or event. The payload is the
   exact text-mode bytes, including internal CRLFs between continuation lines.
   Example: payload `"225-1\r\n225 OK MESSAGE QUEUED\r\n"` is one frame.
   Each 70x event is also one frame (for example `701-1\r\n701-3\r\n701 BEGIN\r\n`).

Verified: text and binary clients behave identically (same replies, same events).

Rust notes: one `tokio` task reads frames and splits them into reply or event by
the first 3 digits. A `7xx` frame is an event and anything else is the reply to the
oldest outstanding command. Replies are strictly in command order. Events can
interleave between a command and its reply.

## 3. Session handshake

None is required. `HELLO` returns `200 OK HELLO` and is stateless. Recommended
sequence:

```
SET self CLIENT_NAME gorca:main:main     -> 208 OK CLIENT NAME SET   (name is cosmetic, not stored for routing)
HISTORY GET CLIENT_ID                    -> 245-<id> / 245 OK CLIENT ID SENT
X-CAPABILITIES                           -> 281-... / 281 OK CAPABILITIES
```

`SET self NOTIFICATION all on` is accepted (`220`) but ignored. Events (700..703) are
always delivered for every message. In speech-dispatcher they are opt-in, so do not
rely on this.

## 4. Verbs (what actually works)

Legend: OK = real behaviour, ACK = accepted, no effect, SELF = scope token
ignored, behaves as `self`.

| Verb | Reply | Status |
|---|---|---|
| `SPEAK` | `230`, then (after data) `225-<id>` / `225 OK MESSAGE QUEUED`. Errors: `502 ERR QUEUE FULL`, `502 ERR SERVER SHUTTING DOWN`, `502 ERR INTERNAL` | OK |
| `CHAR <c>`, `KEY <k>`, `SOUND_ICON <n>` | same as SPEAK (`225` reply, then events) | ALIAS: the argument is spoken as plain text. No char or key naming, no icons |
| `STOP [scope]`, `CANCEL [scope\|id]` | `210 OK STOPPED` / `213 OK CANCELED` | SELF, always cancels everything of this connection (all triples). `CANCEL <id>` is NOT per-message |
| `PAUSE`, `RESUME` | `211` / `212` | ACK only |
| `BLOCK BEGIN/END` | `260` / `261` | ACK only |
| `SET <scope> <param> <value>` | per-param `2xx`, see below | SELF |
| `GET <param>` | `251-<value>` / `251 OK GET RETURNED` | OK for the params listed below |
| `LIST OUTPUT_MODULES` | `250-<engine>` per line, `250 OK MODULE LIST SENT` | OK. "Output module" means engine (espeak-ng, piper, ...), not audio sink |
| `LIST SYNTHESIS_VOICES` | `249-<name>\t<lang>\t<variant>` per line, `249 OK ...` | OK, about 14.8k lines for espeak-ng (variants are listed as languages) |
| `LIST LANGUAGES`, `LIST VOICES` | `247-...`; `LIST VOICES` returns the 8 fixed symbolic types (per code comments, `session.go:1494-1517`) | not exercised in this audit |
| `HISTORY GET CLIENT_ID` | `245` | OK. Other `HISTORY` forms return `299 OK BUT NOT IMPLEMENTED` |
| `HELP`, `QUIT`/`BYE` | `248...`, `231 HAPPY HACKING` | OK |
| `X-PING [tok]` | `280 PONG [tok]` | OK, usable pre-HELLO |
| `X-CAPABILITIES` | `281-key: value ...` / `281 OK CAPABILITIES` (keys: version, framing, transport, mtls, engine, extension) | OK |
| `X-ENGINES` | `282-<name> <state flags>` / `282 OK ENGINE LIST SENT` | OK |
| `X-LANGUAGES [engine]`, `X-VOICES [engine]` | `283` / `284` | OK |
| unknown verb | `420 ERR UNKNOWN COMMAND` | OK |

SET parameters (value; reply):
`CLIENT_NAME` (`208`), `PRIORITY important|message|text|notification|progress`
(`202`; invalid gives `418`), `OUTPUT_MODULE <engine>` (`216`; unknown engine gives
`422` immediately; this selects the engine, the audio sink is global),
`LANGUAGE` (`201`), `VOICE`/`SYNTHESIS_VOICE` (`209`), `VOICE_TYPE` (`209`),
`RATE`/`PITCH`/`PITCH_RANGE`/`VOLUME` (integer, clamped to -100..100, replies
`203`/`204`/`263`/`218`; non-integer gives `428`/`429`/`437`/`430`),
`PUNCTUATION none|some|most|all` (`205`), `SPELLING on|off` (`207`),
`CAP_LET_RECOGN` (`206`, stored verbatim, unused), `SSML_MODE on|off` (`219`),
`NOTIFICATION`, `DEBUG`, `PAUSE_CONTEXT`, `HISTORY` (ACK only, value discarded).
Unknown param gives `410 ERR INVALID PARAM`.

Numeric error codes for COULDNT_SET differ from speech-dispatcher
(4xx instead of 3xx, `docs/WIRE_DIVERGENCES.md`). Branch on the class (2/4/5),
not on the exact code.

Default session state: priority `text`, engine/voice/language from `[defaults]`.
Settings are snapshotted at the moment the SPEAK data is received
(`session.go:1194-1200`). A SET sent after a SPEAK never affects that SPEAK, so
ordering is safe.

## 5. Asynchronous events

Always delivered on the connection that issued the SPEAK, one block per event:

```
701-<msg_id>\r\n701-<client_id>\r\n701 BEGIN\r\n
702-<msg_id>\r\n702-<client_id>\r\n702 END\r\n
703-<msg_id>\r\n703-<client_id>\r\n703 CANCELED\r\n
700-<msg_id>\r\n700-<client_id>\r\n700-<mark_name>\r\n700 INDEX MARK\r\n
```

704/705 (PAUSED/RESUMED) are defined but never emitted.

Event semantics (verified in the code):

- `701 BEGIN` is emitted when the first PCM chunk arrives from the engine, just
  before it is written to the sink (`triple.go:752-760`). It is not "audio audible".
  An empty utterance still gets BEGIN then END.
- `700 INDEX MARK` is emitted right after the chunk containing the mark has been
  written to the audio sink (`triple.go:803-806`). It is not emitted when the mark
  is played. The skew is the sink's ring or buffer depth plus any queued audio. The
  mark name is whatever was in the SSML `<mark name="..."/>`. It requires
  `SET self SSML_MODE on`. Verified with espeak-ng: marks arrive in order,
  between BEGIN and END.
- `702 END` is emitted after the sink has drained the utterance (`triple.go:500-517`
  `Drain`, then OnEnd), so it is close to true end of playback. A known issue
  documented in `CHANGELOG.md` (FINDING #3): on ALSA through snd-aloop, drain
  never completes promptly and END is late.
- `703 CANCELED` is emitted for the active message and every queued message of
  every triple of the connection on STOP/CANCEL, and on disconnect.
- Exactly one of END / CANCELED / error terminates each message id.
- Errors after `225` arrive as a bare, id-less reply line, for example
  `502 ERR INTERNAL` or `503 ERR ENGINE_DEAD` (`session.go:1946-1966`). The message id
  is not included, so correlation is by order only. Observed: `SET VOLUME -100` then
  SPEAK gives `701` then `502 ERR INTERNAL`
  (espeak-ng reports `ErrSilentSynth` for all-zero audio). A Rust client must treat
  any unsolicited 5xx line as "terminal failure of the oldest unfinished message".
- Delivery is lossy only when the client stops reading: the per-session writer queue
  holds 512 items and drops on overflow (`session.go:~590-680`). A client that
  reads continuously never loses events.
- Race: the `225` reply is queued after the message has been handed to the worker
  (`session.go:1165-1184`), so on a busy host a `701` can in principle precede its
  `225`. Do not assume 225-before-701. Match events by message id.

## 6. Cancel and interruption semantics

- `STOP` and `CANCEL` call `Dispatcher.CancelClient(self)` (`session.go:817-842`).
  All pending messages of all (client, engine, voice) triples are dropped with `703`,
  and the active stream context is cancelled. The `210` reply is sent immediately,
  before the audio device has been silenced.
- Measured (null sink, espeak-ng, load average about 10): `STOP`->`210` p50 242 us,
  p99 771 us; `STOP`->`703` p50 306 us, p99 804 us, max 5.6 ms. With a real sink,
  add the device buffer: ALSA example config `buffer_us = 30000`, JACK one period
  (about 21 ms at -p1024 / 48 kHz). Estimated, not measured.
- **Known defect (reproduced): speech sent right after STOP can be lost.**
  `CancelClient` closes each worker's `quit` channel but the worker only leaves the
  map after its current request has unwound (`triple.go:95-110`, `144-160`,
  `dispatch.go:494`). A SPEAK enqueued in that window lands in the dying worker's
  queue and is drained as `703 CANCELED` (`triple.go:222-260`). Reproduction: pipelined
  `STOP; SPEAK` lost 65 of 100 new messages with an instant sink. With an artificial
  3 ms sink-cancel delay, `STOP`, wait for `210`, then `SPEAK` lost 60 of 60.
  Workaround for the client until fixed: after `STOP`, wait for the `703` of the
  active message (or a short fixed delay) before sending the next SPEAK. This
  is the central interrupt-and-speak pattern of a screen reader, so voicego must be
  fixed before gorca depends on it (see the audit report, item P0-1).
- Priorities (`important > message > text > notification > progress`) only reorder
  messages that are queued in the same triple (`popHighest`, `triple.go:916`). There is
  no preemption of the running message and none of the speech-dispatcher interrupt rules
  (`doc/ssip.texi:571-650`). Cross-triple ordering is serialised by a
  per-engine mutex (`dispatch.go:561`). gorca must do its own interruption with
  `STOP` before `SPEAK`.

## 7. Request throughput and limits

- Per-triple queue: 64 messages (`ErrQueueFull` gives `502 ERR QUEUE FULL`).
- A triple is `(client_id, engine, voice)`. Changing the voice between two messages
  routes them through different worker goroutines. In a 40-message alternating-voice
  test, begin order matched queue order (3 runs, 20 ms simulated drain). Ordering across
  triples is not guaranteed by design.
- Measured latency (null sink, real libespeak-ng, RT kernel, load average about 10,
  n=500 each): `data` (last byte of SPEAK body sent) to `225`: p50 0.11 ms, p99 0.72 ms;
  to `701 BEGIN`: p50 2.06 ms, p99 3.40 ms, max 3.93 ms (text) and p50 2.01 ms, p99 3.26 ms
  (binary); to `702 END` (short text): p50 13.7 ms, p99 18.5 ms.
  Text and binary framing are indistinguishable.

## 8. Voice, rate, pitch

- Wire scale -100..100 for rate, pitch, pitch_range, volume, forwarded unchanged to
  the engine plugin, which maps to native units (`docs/SPEC_V2.md` section 1.7). Out of range
  values are clamped (RATE 500 gives GET RATE 100).
- `SET VOICE <name>`: engine-specific voice name (espeak-ng: `en-us`, `English (America)`,
  ...). Unknown voices or languages are silently replaced at SPEAK time by the engine's
  catalog default (`session.go:1213-1300`), with no error to the client. Gotcha:
  `SET LANGUAGE en` with the stock espeak-ng config resolves to the voice
  `English (Caribbean)` (observed in the debug log). gorca must set explicit
  voice names from `LIST SYNTHESIS_VOICES` or `X-VOICES espeak-ng`.
- There are no per-SPEAK parameters. Every change is a `SET` round trip (about 0.1-0.2 ms
  each on the UNIX socket). Replies are in order, so a client may pipeline
  `SET ... SET ... SPEAK` without waiting.
- SSML is supported when `SSML_MODE on` (marks verified). Only `<mark>` was tested.
