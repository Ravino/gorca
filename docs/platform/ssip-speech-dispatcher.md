# SSIP (speech-dispatcher 0.12.0) as used by gorca

Sources: `refs/speech-dispatcher/doc/ssip.texi`, `refs/speech-dispatcher/src/modules/espeak.c`
(the `sd_espeak-ng` module is built from it), live private instance.
Host: `speech-dispatcher` 0.12.0-5, modules `sd_espeak-ng`, `sd_espeak-ng-mbrola`, `sd_generic`, `sd_dummy`;
the user's daemon is `/usr/bin/speech-dispatcher -s -t 0` (socket-activated, never exits) with
`AudioOutputMethod pulse` in `~/.config/speech-dispatcher/speechd.conf`.

## Transport
* Unix socket `$XDG_RUNTIME_DIR/speech-dispatcher/speechd.sock` (live: `/run/user/1000/speech-dispatcher/speechd.sock`);
  inet socket optional. Text protocol, CRLF line ends, one reply per command (`NNN-` continuation
  lines, final `NNN text`). Commands: `SET SELF|ALL|<id> <PARAM> <value>`, `SPEAK` (reply `230` then
  text lines, terminated by a line `.`; reply `225-<msg_id>` + `225 OK MESSAGE QUEUED`), `CHAR`, `KEY`,
  `SOUND_ICON`, `STOP|CANCEL|PAUSE|RESUME {self|all|id}`, `BLOCK BEGIN/END`, `LIST ...`, `QUIT`.
* Start with `SET SELF CLIENT_NAME user:program:component`; `SET SELF NOTIFICATION ALL on` enables events.
* Events are asynchronous multi-line replies on the **same connection** (`701-<msg_id>`, `701-<client_id>`,
  `701 BEGIN`; `702 END`, `703 CANCELED`, `704 PAUSED`, `705 RESUMED`, `700 INDEX_MARK`); the spec
  guarantees they never interleave between a command and its reply. A client must demultiplex them.
* Per-message parameters (rate, pitch, volume, voice, language, punctuation, priority, SSML mode)
  are sticky connection state set by separate `SET` commands, each a round trip unless pipelined.

## Priorities (ssip.texi l.563-657, behaviour confirmed with the private instance)

| Priority | Behaviour |
|---|---|
| `IMPORTANT` | spoken immediately, never interrupted, also not by another IMPORTANT (those queue in order); cancels a playing lower-priority message, postpones MESSAGE/TEXT, cancels NOTIFICATION/PROGRESS. |
| `MESSAGE` | queued behind IMPORTANT/MESSAGE; does not interrupt itself; cancels TEXT/NOTIFICATION/PROGRESS. |
| `TEXT` | queued behind IMPORTANT/MESSAGE; **interrupts itself** (only the latest TEXT survives); cancels NOTIFICATION/PROGRESS. |
| `NOTIFICATION` | dropped if anything else is queued or playing; interrupts itself. |
| `PROGRESS` | like NOTIFICATION but does not interrupt itself; the last one of a series is promoted to MESSAGE. |

Implication: for a screen reader that wants "newest wins", use `TEXT` (self-interrupting) or send an
explicit `CANCEL self` before each new utterance; `IMPORTANT` alone queues instead of replacing.
`STOP` ends the current message only; `CANCEL` also discards the queued ones; `PAUSE/RESUME` keep them.

## Cancel semantics inside the espeak-ng module (source)
`module_stop()` sets `stop_requested`; the synth callback (`synth_callback`, called per audio chunk of
`EspeakAudioChunkSize` ms, default 300 in `/etc/speech-dispatcher/modules/espeak-ng.conf`) polls
`module_process()` for new commands, returns 1 to abort synthesis, and the audio thread flushes the sink.
`BEGIN` is reported from the first synth callback (`module_report_event_begin`), i.e. **before** audio
reaches the sound card; it does not include audio-stack latency.

## Measurements (2026-10-12)
Method: private speechd (`speech-dispatcher -s -t 0 -S <sock> -C <private conf>`, espeak-ng module,
`AudioOutputMethod alsa`, `AudioALSADevice null` so nothing is audible and the sink never blocks).
The user's own speechd/Orca were not touched. Scripts: `tools/platform-probes/ssip_lat.py`,
`ssip_prio.py`. RT kernel, idle desktop, Python client.

| Metric | Result |
|---|---|
| `SPEAK` "a" -> `225 OK MESSAGE QUEUED` | p50 0.10 ms, max 0.84 ms (30 samples) |
| `SPEAK` "a" -> `701 BEGIN` event | min 0.2, p50 0.7, p90 1.1, max 3.3 ms (30 samples, idle module) |
| `CANCEL self` ack | p50 0.09 ms, max 0.24 ms |
| `CANCEL self` -> `703 CANCELED` event | p50 3.6 ms, max 13.7 ms (10 samples, 8-sentence message) |
| `CANCEL self` + new `SPEAK` (TEXT) -> `701 BEGIN` of the new message | p50 13.1 ms, max 20.5 ms (20 samples) |
| TEXT interrupted by TEXT: new `BEGIN` after the SPEAK reply | ~12-13 ms; IMPORTANT over TEXT ~8 ms |

Not covered / caveats: (1) real audio latency is not included: `AudioPulseMinLength` default 10 ms and
PipeWire `clock.quantum` is 1024 @ 48 kHz = 21.3 ms on this host (`pw-metadata -n settings`), so
expect ~10-30 ms more until sound; (2) the null sink makes playback instantaneous, so the
"MESSAGE then TEXT" rows of `ssip_prio.py` do not exercise real interruption; (3) numbers are
client-side wall-clock from Python and include its overhead.

Take-away: gorca-side SSIP command latency is negligible (<1 ms), but the daemon adds ~13 ms p50
(up to 20 ms) to a cancel-and-replace sequence, and `BEGIN` events are the only feedback.
