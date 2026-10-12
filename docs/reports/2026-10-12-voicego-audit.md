# voicego audit for gorca (v2.0.4, commit 3a0f1f2, AGPL-3.0)

Author: go-engineer, 2026-10-12. Source: `refs/voicego` (unmodified). Scratch copy,
test client and probes: `.toolchain/scratch/{voicego,lat,probe}` (gitignored).
Wire details: `docs/platform/voicego-protocol.md`.

> Status update: P0-1 to P0-4 and the `SET VOLUME -100` failure are fixed on branch
> `fix/p0-stability` of `refs/voicego` (not merged). See
> `docs/reports/2026-10-12-voicego-p0-fixes.md`. Findings below describe v2.0.4.

## 0. Verdict

voicego is a sound base for speech-as-a-service (about 77k lines Go, large test
suite, engines isolated in subprocesses, 3 ms SPEAK-to-first-audio). It is not
ready as gorca's primary backend. The headline issues:

1. There is no native protocol. The "binary protocol" is SSIP with a length prefix.
2. A reproducible bug loses speech sent right after STOP (P0-1).
3. One goroutine panic anywhere kills the whole server (P0-2).
4. The audio path is unproven on this machine (PipeWire/GNOME): no PipeWire sink, the
   real ALSA and JACK sinks need dev packages we do not have installed, and a sink
   that fails once never recovers.

## 1. Architecture map

### 1.1 Process model

```
SSIP client --unix/tcp/mTLS--> voicegod (one Go process)
                                 transports -> session (per conn) -> Dispatcher
                                 -> tripleWorker (per client,engine,voice)
                                 -> gRPC (go-plugin, UDS) -> voicego-engine-<name> (own process)
                                 -> sink (in-process, cgo: libasound / libjack / libsamplerate)
```

- `cmd/voicegod/main.go`: config, supervisor boot, transports, optional pprof,
  SIGHUP (cert reload), SIGUSR2 (pprof bundle to `/tmp`).
- Engines (espeak-ng, espeak, rhvoice, piper, stub) are separate executables
  `voicego-engine-<name>`, discovered in `plugin_dir`, `/usr/libexec/voicego`,
  `/usr/local/libexec/voicego` (`docs/SPEC_V2.md` section 4). Handshake by go-plugin
  magic cookie. State machine REGISTERED/DISCOVERED/HANDSHAKING/READY/CRASHED/RESTARTING/DEAD
  (`internal/plugin/state.go`). Restart backoff 1/2/4/8/16 s, 5 attempts, then DEAD until
  `voicegod` restarts (`SPEC_V2.md` section 6). The espeak-ng plugin boots and warms in
  about 4 ms (observed in logs).
- Sinks are NOT isolated: ALSA, JACK2 and the sample-rate converter are cgo inside
  `voicegod`. A native crash in a sink takes the server down (the JACK RT heap-corruption
  history, `JACK_RT_BUG.md`, is the example; its root cause, `jack_ringbuffer_reset` from
  the Go thread, is gone from the code: grep finds only comments).

### 1.2 Concurrency

| Goroutine | Owner | Evidence |
|---|---|---|
| accept loop per transport | `Server.Serve` | `internal/server/server.go:270` |
| session read loop (one per connection) | server | `server.go:~358` |
| session writer (one per connection), channel cap 512, non-blocking enqueue with drop counters | session | `session.go:484-520`, `605-640` |
| tripleWorker (one per (client, engine, voice); idle TTL 30 s; queue 64; 5 priority lanes) | Dispatcher | `internal/dispatch/triple.go:74-220` |
| Recv pump per active stream | worker | `triple.go:663` |
| fire-and-forget plugin `Cancel` RPC (500 ms bound) | worker | `triple.go:562-580` |
| per-engine gate (mutex) serialises Synthesize across ALL clients of one engine | Dispatcher | `dispatch.go:561` |

So the "parallelism per triple" in the README is only parallelism between engines;
within an engine, synthesis is serial by design (a mutex held through sink drain).
This is fine for a screen reader.

Deadlines: stream first-chunk 12 s, inter-chunk 3 s, then engine kill and restart
(`triple.go:612-720`). The RPC deadline for control calls is bounded. Idle read
deadlines exist; there is no write deadline on client connections (only on one TLS
path, `tls_transport.go:415`).

### 1.3 Audio path

Engine plugin streams PCM chunks (own rate and format) via gRPC. The worker writes
each chunk to the sink as it arrives (HTTP/2 flow control gives backpressure), emits
701 on the first chunk, 700 for marks, then `Drain` and 702.

Sinks (`plugins/output/`): `null`, `alsa` (libasound writei, `snd_pcm_wait` 100 ms
poll, XRUN recovery, open retry on EBUSY/EBADFD 100/250/500 ms, libsamplerate
resampler, config example period 10 ms / buffer 30 ms), `jack2` (libjack, ring buffer
with an RT callback, per-stream libsamplerate). There is no PipeWire sink and no Pulse.
On GNOME 48 / Debian 13 the options would be ALSA `default` (pipewire-alsa) or libjack
(pipewire-jack). Neither was tested here. The author's reference setup is jackd on an
exclusive `hw:1` (`LISTENING.md`), so documented ALSA artifacts (clicks, echo,
`ALSA_ARTIFACTS.md`, `ALSA_ECHO.md`) were diagnosed on an aloop bridge and may or may
not apply.

Default builds use stub backends (`backend_stub.go`). Real backends need build tags
`alsa_real`, `jack_real`, `espeak_ng_real`, etc. On this host `pkg-config` finds no
alsa or jack (dev packages missing), but `libespeak-ng` headers are present, so the
espeak-ng plugin was built real and the sink was null.

### 1.4 Plugins

Engine plugins only (no audio plugins, no scripting). Contract: `pkg/api/engine/v1/engine.proto`
(GetInfo, ListLanguages, ListVoices, Validate, Synthesize server-stream with PCM +
IndexMark events, Cancel, Shutdown). SDK in `pkg/sdk/engine`. Third-party engines in any
language that can speak gRPC are possible.

### 1.5 Transports and protocol layers

See protocol doc. `SPEC.md` section 2.2 ("binary protocol, v2, deferred, framing-only
delta") matches the code. `AUDIT.md` (2026-05-24) is obsolete: it describes the
pre-rewrite tree (`internal/binproto`, `aplay`-based audio, `internal/queue`, scheduler);
none of those packages exist now. `SSIP_VERBS.md` is a Phase-2 snapshot and is stale in
places: `LIST` is no longer a stub (works), `X-ENGINES/X-LANGUAGES/X-VOICES` exist, and
`SET CLIENT_NAME` is stored as display name only. Verbs, replies and events in the
protocol doc come from the code and runs, not from these files. `docs/ARCHITECTURE_V2.md`
and `SPEC_V2.md` match the code I read (gRPC layer, supervisor). `DESIGN_*` documents
are design history; I did not verify them line by line.

## 2. Stability hunt

### 2.1 Test and tooling results

- `go build ./...` OK. `go vet ./...` clean (no output).
- `go test -race -count=1 ./...`: all 26 packages with tests pass (cmd/voicegod 71 s).
  `-count=5` on dispatch/server/ssip also passes. Note: default tags only, so ALSA, JACK
  and all real engine code are NOT covered by this run (stubs).
- `go test -race -tags espeak_ng_real ./cmd/voicego-engine-espeak-ng`: FAIL
  `TestReal_Params` (`backend_real_test.go:216`):
  `espeak-ng: synthesizer produced no audio for non-empty text (... max_abs=0)` for
  rate/pitch/volume = -100. Cause: SET VOLUME -100 maps to silent audio, which
  `ErrSilentSynth` (`cmd/voicego-engine-espeak-ng/errors.go:35`) turns into an error.
  Reproduced end to end: `SET VOLUME -100` then SPEAK gives `701` then a bare
  `502 ERR INTERNAL`. Minor for gorca (it never sets -100), but it is a real test
  regression and shows the real-backend suite is not run in their gate.

### 2.2 Ranked findings

Severity reflects impact on a screen reader. "Repro" = I reproduced it; "code" =
read in code only.

**P0-1. Speech lost after STOP (repro).** `STOP` closes the worker's `quit` channel
(`triple.go:95-98`) and the worker deregisters only after the current request unwinds
(`triple.go:152-158`, `dispatch.go:494`). A SPEAK arriving in between is accepted into
the dying worker's channel and drained as `703` (`triple.go:222-260`).
Numbers: pipelined `STOP; SPEAK` 65/100 new messages cancelled (instant null sink); with
a 3 ms injected sink-cancel delay, `STOP`, wait for `210`, `SPEAK` lost 60/60. A real
sink cancel (`snd_pcm_drop`, plugin Cancel RPC, gate release) takes this long or longer.
Fix: mark the worker not-alive under `d.mu` in `cancelClient` (before closing `quit`) so
`Enqueue` spawns a fresh worker, or give workers a generation counter and make cancel
per-generation.

**P0-2. No panic containment (code).** The only `recover()` in production code is around
EventSink callbacks (`triple.go:995`). There is none in `session.run`
(`server.go:358`), `tripleWorker.run/process/streamChunks` (`triple.go:144-700`), the
Recv pump, the sink writers, the supervisor goroutines or the cgo callbacks. Any panic
(nil, index, closed channel) in any of them terminates `voicegod`, which contradicts the
"degrades only its own actor" goal. Fix: `defer recover` at every goroutine entry that
converts to OnError and keeps the process alive.

**P0-3. Sink open failure is permanent (code).** `sinkCache.getOrOpen` uses a
`sync.Once` per module and caches `entry.err` forever (`internal/dispatch/sink.go:123-137`).
Also the first caller's per-request `ctx` (`streamCtx`) is passed to `plugin.Open`
(`sink.go:131`), so a STOP during the very first open (lazy open, because boot prewarm
is not wired, `main.go:329-347`) can cache `context canceled` as the sticky error. Once
the device is missing or busy at first use (PipeWire not up, EBUSY), every later SPEAK
fails with 502 until restart.

**P0-4. No device recovery after sink loss (code).** `ErrDeviceGone` comment says
"dispatcher will reopen" (`plugins/output/alsa/errors.go:39`), but nothing in
`internal/dispatch` reopens or invalidates a sink (no occurrence of
`ErrDeviceGone`/reopen in dispatch). After an ENODEV/EBADFD that `recover()` cannot fix
(PipeWire restart, headset unplug) the cached sink stays broken. Likely the dominant
real-world "crashed sometimes" for a desktop that restarts its audio server.

**P1-1. Unattributed async errors (repro).** Failures after `225` are bare
`502 ERR INTERNAL` / `503 ERR ENGINE_DEAD` lines with no message id
(`session.go:1946-1966`). A multiplexing client cannot tell which message failed.

**P1-2. 225/701 ordering race (code).** `enqueueSpeak` is called before `replyQueued`
(`session.go:1165-1184`); a fast worker can queue 701/702/502 first. Not observed in 1500
iterations on a loaded box (225 about 0.1 ms vs 701 about 2 ms) but possible.

**P1-3. DEAD engine is terminal (design).** After 5 failed restarts the engine stays
dead until `voicegod` restart (`SPEC_V2.md` section 6). For a single-engine screen
reader that means permanent silence. A restart counter reset window or an admin verb is
needed.

**P1-4. Cancel race with plugin wedge (code, mitigated).** Cancelling a stream can wedge
a CGo plugin; the code compensates with a plugin Cancel RPC, bounded Recv deadlines and
engine restart-on-wedge (`triple.go:562-720`). The comments document several previous
wedges (2026-06-05..07). Treat the cancel path as the least-trusted area.

**P2-1. Sink in the daemon process (design).** See 1.1. A cgo crash kills the server.
The supervisor does not restart voicegod; systemd would have to.

**P2-2. Slow-reader session wedge (code).** `cleanup()` waits for `writerDone` before
closing the connection (`session.go:738-765`). A writer blocked in `conn.Write` on a
stalled peer never returns (no write deadline), so that session goroutine stays until
the process shuts down. Bounded impact (one goroutine, other clients unaffected), but
it blocks `Server.Shutdown`'s `wg.Wait` until `sess.conn.Close()` from Shutdown.

**P2-3. Voice fallback surprise (repro).** `SET LANGUAGE en`/`VOICE en` resolves to
`English (Caribbean)` (`session.go` catalog chain; debug log). Silent substitution on
unknown voice, no error to the client.

**P2-4. Dead or stale docs.** `AUDIT.md` and parts of `SSIP_VERBS.md` describe code that no longer exists; a new maintainer would
be misled.

**P3. Misc.** `CANCEL <id>` is not per-message; PAUSE/RESUME/BLOCK are stubs;
`CHAR`/`KEY`/`SOUND_ICON` are plain speech; `X-ENGINES` etc. are the only extensions.
`VOICEGO_SEGMENT_SPEAK` segmentation is off by default pending a known 502 regression
(`triple.go:~320`). `SIGUSR2` writes profiles into `/tmp`. Text SSIP detection peeks
3 bytes and blocks until they arrive (infinite idle on UNIX).

### 2.3 What I could not verify

- Any real-audio behaviour: ALSA/JACK sinks, PipeWire, cancel-to-silence at the device.
  `libasound2-dev`/`libjack-dev` are not installed (user must apt install).
- Piper/RHVoice/espeak classic real backends (no libs / not requested).
- Long soak (hours), memory growth, goroutine leaks. Only about 3000 requests were run.
- Whether the user's "crashed sometimes" is P0-2/P0-3/P0-4 or an engine-plugin native
  crash: nothing in the repo records the crash causes. Ask the user for the journal.

## 3. Latency (measured)

Setup: private `voicegod` (scratch build), real `libespeak-ng` plugin, null sink, UNIX
socket, RT kernel 7.1.8, load average about 10 (other jobs running; numbers are
conservative). Client: `.toolchain/scratch/lat/main.go`. Time zero = instant the last
byte of the SPEAK body is written.

| Metric | Text SSIP | Binary framing |
|---|---|---|
| to `225 QUEUED`, n=500 | p50 0.11 ms, p99 0.72 ms | p50 0.11 ms, p99 0.67 ms |
| to `701 BEGIN` (first audio chunk handed to sink), n=500 | min 1.30, p50 2.06, p90 2.68, p99 3.40, max 3.93 ms | min 1.21, p50 2.01, p90 2.59, p99 3.26, max 3.67 ms |
| to `702 END` (short text, null sink drain) | p50 13.7, p99 18.5 ms | p50 13.7, p99 18.8 ms |
| STOP to `210` (n=100, text) | p50 0.24, p99 0.77, max 5.6 ms | p50 0.20, p99 0.75 ms (n=100, earlier run) |
| STOP to `703` | p50 0.31, p99 0.80, max 5.6 ms | p50 0.28, p99 0.77 ms |

Conclusions:

- Binary framing buys nothing measurable over text (as expected: same parser).
- voicego-side speak-to-first-sample is about 2 ms p50, 3.4 ms p99. That fits the 15 ms
  p99 budget with room, provided the sink is already open. Lazy sink open on the first
  utterance is not measured here (ALSA open retries 100/250/500 ms on EBUSY).
- Cancel-to-silence at the device was NOT measured. Expected = 0.3 ms processing plus
  device buffering (ALSA example 30 ms, JACK one period about 21 ms). `snd_pcm_drop` or
  `setActive(false)` are immediate in design.
- I did not measure speech-dispatcher as a baseline (not asked, would require a second
  audio path).
- The user's running speech-dispatcher and real audio device were never touched.

## 4. gorca client requirements and gaps

Needs (from Orca's use of speech-dispatcher, `refs/orca/src/orca/speechdispatcherfactory.py`
and `refs/speech-dispatcher/doc/ssip.texi`): per-utterance rate/pitch/volume/voice/language
(Orca re-applies ACSS before every speak, lines 219-318), SSML with `<mark>` and BEGIN /
END / CANCEL / INDEX_MARK callbacks (say-all, lines 335-376), cancel (line 390), `char`
and `key`, list voices, priority MESSAGE as default.

| Need | voicego today | Gap |
|---|---|---|
| Fast native protocol | SSIP text or length-framed SSIP. Same semantics and speed | G1: no real native protocol; per-SPEAK params, request ids, typed events do not exist |
| Cancel then speak | 210 immediately, 703 per message, but the next SPEAK can be lost | G2 = P0-1 |
| Per-message cancel, cancel-all-clients, cancel by scope | `CANCEL id`/`STOP all` collapse to self | G3 |
| Priorities and interruption | Lane ordering inside one triple only; no interrupts, no progress/notification drop rules | G4. gorca can implement policy itself by STOP before SPEAK once G2 is fixed |
| Index marks for say-all and cursor | 700 via SSML marks, but emitted when the audio is written to the sink, not when played; no sample or time offsets | G5: needs playback-time marks (ring-buffer position) or at least a documented skew |
| Reliable correlation of errors | bare 5xx lines, no id | G6 = P1-1 |
| Voice/rate/pitch switching | SET commands, snapshotted at SPEAK, clamped -100..100. Round trip per change | G7: no per-SPEAK params; unknown voice silently replaced |
| Spelling, char and key semantics, capitalisation, punctuation | punctuation/spelling stored and forwarded to the plugin; CHAR/KEY are plain speech; CAP_LET_RECOGN ignored | G8: gorca can synthesise its own text for chars/keys, so low |
| Pause/resume (interrupt and resume reading) | stub | G9: low for MVP |
| Audio on GNOME/PipeWire | no PipeWire sink; ALSA default or pipewire-jack untested | G10 |
| Sink and engine health | engine isolated; sink in process; no sink reopen; DEAD terminal | G11 = P0-3, P0-4, P1-3 |
| Unix socket / auth | UNIX socket, no peer credential check | OK |
| Event delivery guarantees | lossy only if the client stops reading | OK for a Rust reader task |
| Sound icons | spoken as text | gorca plays its own earcons, so low |

Latency target: keypress to speech-start p99 < 15 ms on the gorca side. voicego adds
about 2 to 3.4 ms (measured) between SPEAK and first PCM handed to the sink, plus device
buffer. Achievable if the sink is pre-opened and G2 is fixed.

## 5. Prioritised fix and feature list

P0 (blockers before gorca depends on it):

1. P0-1: STOP/SPEAK race. Small change (alive flag or generation under `d.mu`). Add a
   regression test: `STOP` + immediate `SPEAK` with a 5 ms sink cancel, 1000 iterations.
2. P0-2: panic containment at every goroutine entry (session, worker, pump, supervisor,
   sink writers), mapping to OnError and metrics. Add a test that injects a panic
   engine and sink.
3. P0-3/P0-4: sink lifecycle. Do not cache errors or per-request contexts. Prewarm the
   sink at boot (`PrewarmSink` exists, `main.go:329-347`). Invalidate and reopen the sink
   on `ErrDeviceGone` / `ErrBadState` / repeated write errors, with backoff. Probe the
   device when the engine goes READY.
4. Run the real-backend test suite (`-tags alsa_real,jack_real,espeak_ng_real`) in CI and
   fix `TestReal_Params` (treat volume -100 as legal silence, or reject it with a clear
   error code).
5. Audio on this platform: add a PipeWire-native sink (or validate ALSA to pipewire-alsa
   and pipewire-jack on GNOME 48) and measure cancel-to-silence on the real device.
   Needs the user to install `libasound2-dev libjack-jackd2-dev libsamplerate0-dev`
   (apt run by the user).

P1 (needed for a screen-reader-grade protocol):

6. A native, versioned binary protocol (extension of the existing framing is fine):
   request id chosen by the client and echoed in every reply/event/error; per-SPEAK
   parameter block (voice, lang, rate, pitch, volume, priority, ssml flag, punctuation);
   typed error events carrying the id; `CANCEL id` and `CANCEL all-mine`; `SPEAK` with
   "supersede": atomic cancel-then-speak in one command (removes G2 by construction
   and saves a round trip).
7. Priority policy as in `ssip.texi` (important interrupts, text supersedes text,
   notification/progress dropped), implemented in the dispatcher.
8. Playback-accurate index marks (position derived from the sink's consumed frames) and
   optional sample offsets in BEGIN/END.
9. Fix 225-before-event ordering (enqueue the reply before the worker can emit).
10. Engine DEAD recovery: reset attempts after a quiet period, add `X-ENGINE_RESTART`.
11. Write deadline on client connections and decouple writer teardown from connection
    close.
12. Replace silent voice substitution with an explicit "voice not found, using X"
    notice, or an error mode for gorca.

P2:

13. Real PAUSE/RESUME/BLOCK.
14. Run sinks out of process (or at least guard cgo with a watchdog and systemd restart).
15. Delete or refresh stale docs (`AUDIT.md`, `SSIP_VERBS.md`).
16. Per-engine parallel synthesis (reentrant engines) if ever needed.

## 6. Options for the gorca team

- A. Adopt voicego, fix upstream (user owns it): P0 list is about 1-2 weeks of Go work,
  P1 protocol a further 2-3 weeks. Keeps AGPL-3.0 (gorca must be compatible if linked;
  as a separate process over a socket it is not linked).
- B. Use voicego as a library of ideas and write a new speech service in Rust: larger
  cost, loses the engine plugin ecosystem.
- C. Keep speech-dispatcher/SSIP as the only backend for now, add voicego behind the
  same client trait after P0-1..P0-4 land.

Recommendation: A, executed behind a `SpeechBackend` trait in gorca with speech-dispatcher
as the compatibility implementation (option C as a transition). Confidence: medium. The
measured latency and engine isolation are good, and the defects are local and fixable.
Confidence is not high because the audio path was not exercised on this platform and the
cause of the user's past crashes is unknown.
