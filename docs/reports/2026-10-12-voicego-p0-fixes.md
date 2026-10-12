# voicego P0 fixes (branch fix/p0-stability)

Author: go-engineer, 2026-10-12. Repo: `refs/voicego`, branch `fix/p0-stability`
from `main` (3a0f1f2, v2.0.4). Nothing merged or pushed. origin set to
`git@github.com:Ravino/voicego.git`, history unshallowed, local git identity
Evgeniy Nekrasov. Audit: `docs/reports/2026-10-12-voicego-audit.md`.

## Commits

| Hash | Subject |
|---|---|
| 5428bbe | style: gofmt files that were not formatted |
| 2e45cd9 | fix(dispatch): do not lose SPEAK sent right after STOP (P0-1) |
| 6cf0538 | fix: contain panics at every goroutine boundary (P0-2) |
| c0a44a1 | fix(dispatch): do not cache sink open failures or poison the open context (P0-3) |
| d63f084 | fix(dispatch): reopen the sink after a device error (P0-4) |
| 02b62ee | fix(espeak-ng): treat volume -100 as requested silence, not a synth failure |
| 58f0908 | docs: describe panic containment, sink lifecycle and STOP/SPEAK semantics |
| 0229455 | fix(dispatch): report a STOP during Synthesize start as cancelled, not 502 |
| b0f8d1e | docs: changelog entry for the Synthesize-start cancellation fix |

All authored as Evgeniy Nekrasov <evgeniy@nekrasov.pw>, no trailers, no AI
attribution (checked with grep over the commit messages). 30 files, +1324/-134
(18 non-test, non-doc files, +543/-127). The first commit exists because six files were
not gofmt-clean at baseline and the gate requires `gofmt -l` to be empty.

## What changed

**P0-1 STOP/SPEAK race.** `tripleWorker.cancelClient` now calls `Dispatcher.deregister`
(marks the worker not alive and removes it from the map under `d.mu`) before closing
`quit`. A later Enqueue spawns a fresh worker. Messages accepted before the STOP are
still drained as 703. Test `TestStopThenSpeakNotLost` (`internal/dispatch/stop_speak_test.go`):
pipelined, pipelined with 3 ms sink cancel, and SPEAK 4 ms after STOP with 10 ms sink
cancel, 40 rounds each. Before the fix it lost 38/40, 40/40 and 40/40. The fake synth
stream now returns the context error on cancel like gRPC does (a clean EOF had hidden
cancellations).

**P0-2 panic containment.** New package `internal/safego` (recover, count, log value and
stack). Applied at: SSIP command handler (client gets `502 ERR INTERNAL`, session
continues), session goroutine, session writer op, accept loop (re-entered), triple
worker loop, per-request `process` (terminal OnError for that message only), Recv pump,
plugin Cancel goroutine, event-sink callbacks (now with stack), plugin manager step
(fed into the normal CRASHED/restart/DEAD path), health watcher, dispense and kill
goroutines, eager boot, ALSA and JACK writer commands (`ErrWriterPanic` to the caller,
which then triggers the sink reopen), voicegod SIGHUP/pprof/Serve. Tests inject panics
into a command handler (`TestSession_HandlerPanicIsContained`), Synthesize, the Recv
pump, a sink Write (`internal/dispatch/panic_test.go`), the ALSA writer
(`plugins/output/alsa/panic_test.go`) and safego itself.
Not covered by an injected-panic test: JACK writer (same code shape as ALSA), plugin
manager `protect`, voicegod main goroutines. Engine plugins are separate processes and
were left alone.

**P0-3 sink open failure.** `sinkCache` entry is now a state machine (closed, opening,
open). `Plugin.Open` runs on its own goroutine under the dispatcher's lifetime context
(10 s bound); callers wait on it with their own context, so a STOP abandons the wait
and not the open (message reported as 703). Failure is remembered only for an
exponential backoff of 100 ms doubling to 2 s; calls inside the window fail fast with
the last error. Tests: `TestSinkOpenFailureIsRetried`, `TestSinkOpenBackoffLimitsAttempts`,
`TestSinkOpenSurvivesStopDuringFirstOpen` (`internal/dispatch/sink_recover_test.go`).
Boot prewarm is NOT wired: it needs a device rate chosen by the owner (existing
"Sink prewarm decision" comment in `cmd/voicegod/main.go`), so it is not trivial. A retried lazy
open removes the permanent-failure part of the problem.

**P0-4 sink reopen.** A Write or Drain error that is not a cancellation calls
`sinkCache.invalidate`: the handle is dropped (closed in the background) and the next
SPEAK reopens through the same path, with the same backoff if the device is still down.
Sink handles are single-use, so `dispatch.Config.SinkFactory` builds a fresh one;
voicegod wires it to `internal/sink/factory`. Without a factory the configured instance
is reused (test fakes). Test `TestSinkReopensAfterDeviceLoss`: device lost, down for two
open attempts, reopened with backoff, old handle closed.

**SET VOLUME -100.** espeak-ng silent-synth guard is skipped when the request maps to
native volume 0. New stub test; `TestReal_Params` now passes on the real backend.

**Found by the soak, fixed (0229455).** A STOP landing while the Synthesize RPC starts
returns a gRPC status `Canceled` that does not wrap `context.Canceled`; the worker
reported a bare `502 ERR INTERNAL` with no terminal event for the message (7 in 2700
stops on the first 5-minute run, and 4 lost messages on the unfixed baseline in the A/B
run below). It is pre-existing, in the same STOP/SPEAK reliability area. Test
`TestStopDuringSynthesizeStartIsCancelled`.

**Docs.** `CHANGELOG.md`, `docs/ARCHITECTURE_V2.md` (panic containment, sink lifecycle,
STOP then SPEAK), one line in `DESIGN_PHASE2.md`. In <repo>:
`docs/platform/voicego-protocol.md` (section 6 defect marked fixed, volume note, banner)
and a status line in the audit report.

## Gates (at b0f8d1e)

| Gate | Result |
|---|---|
| `gofmt -l .` | empty |
| `go vet ./...` | clean |
| `go test -race -count=1 ./...` (default tags) | all packages ok, 0 failures (cmd/voicegod 70 s) |
| `go test -race -tags espeak_ng_real ./cmd/voicego-engine-espeak-ng ./internal/... ./plugins/...` | all ok, includes `TestReal_Params` which failed on 3a0f1f2 |
| 30-minute soak | PASS, details below |

One run of the full `go test -race ./...` at an intermediate commit failed in
`cmd/voicegod` under load average 27 while a soak and latency runs were active; I did
not capture which test. Re-runs of that package passed 4 of 4 and the final full run
passed. Treat it as an unidentified timing flake; a reviewer should run that package
a few times.

alsa_real and jack_real code was not built or run (libasound2-dev, libjack-jackd2-dev
not installed); the sink and writer changes are in tag-independent files.

### Soak (30 minutes, race detector, null sink)

Rig (scratchpad `rig/`, client `soak/main.go`, both outside the repo): `voicegod`
built with `-race`, real espeak-ng plugin, null sink, private UNIX socket, 6
connections. Traffic mix per connection: plain SPEAK; pipelined STOP/CANCEL then
SPEAK; STOP, wait for 210/213, then SPEAK; STOP, wait for the 703, then SPEAK; burst of
5 SPEAKs, STOP, SPEAK; flood of 80 SPEAKs. Checks: every message that got `225`
reaches exactly one terminal event within 30 s (no loss), a SPEAK sent after the last
STOP/CANCEL must end with 702, no unexpected error line, no connection error.

```
FINAL queued=348138 end=312562 canceled=35576 accounted=348138 rejected=66945 stops=17804
      lost=0 mustEnd=0 asyncErr=0 proto=0      client rc=0, daemon alive at end
```

`rejected` are `502 ERR QUEUE FULL` replies from the flood pattern (expected). No
`panic` or `DATA RACE` in the daemon log or in a GORACE log file (none created).
Goroutines flat at 47 from minute 4 to the end. Race-build RSS grew from 171 MB to 313 MB;
a follow-up run without the race detector showed RSS flat (base 43.8 MB, new 45.6 MB over 6
minutes), so I attribute the growth to the detector. Not proven for hours.

A/B on the same soak, 6 minutes, 3 connections, both builds at once (non-race):

| Build | queued | lost | SPEAK after STOP cancelled (mustEnd) | async errors |
|---|---|---|---|---|
| 3a0f1f2 | 63904 | 4 | 1497 | 4 |
| fix/p0-stability | 63356 | 0 | 0 | 0 |

lat tool, 100 iterations of speak long, 20 ms, STOP, pipelined SPEAK: baseline 81/100 new
messages cancelled, branch 0/100. Variant "wait for 210 then SPEAK": 13/100 vs 0/100.
"Wait for 210 and 703": 0 vs 0.

### Latency (null sink, real libespeak-ng plugin, UNIX socket, text SSIP, n=500 per run)

Time zero is the last byte of the SPEAK body. Four interleaved rounds, base then new;
machine load average about 14 (varied 10 to 27 during the session).

| Metric | 3a0f1f2 p50 / p99 (ms) | fix/p0-stability p50 / p99 (ms) |
|---|---|---|
| to `225 QUEUED` | 0.045-0.048 / 0.31-0.36 | 0.046-0.051 / 0.30-0.33 |
| to `701 BEGIN` run1 | 1.770 / 2.431 | 1.753 / 2.368 |
| run2 | 1.743 / 2.443 | 1.774 / 2.511 |
| run3 | 1.722 / 2.449 | 1.755 / 2.416 |
| run4 | 1.752 / 2.462 | 1.742 / 2.481 |
| binary framing | 1.760 / 2.406 | 1.758 / 2.409 |

No regression: p50 within 0.03 ms, p99 within 0.1 ms, both ways. STOP to 210 (n=40,
under load): p50 0.156 ms vs 0.163 ms, p99 0.79 ms vs 0.48 ms; STOP to 703 p50 0.23 vs
0.22 ms. Earlier runs under load 27 had noisy p99 (3 to 10 ms) for both builds equally.
Cancel-to-silence at a real device was not measured (no real sink).

## Risks and open questions for the reviewer

- Reopen after a device error closes the old handle in the background while the new one
  opens. On an exclusive `hw:` ALSA device the new open can hit EBUSY until the close
  completes; the ALSA sink already retries EBUSY at 100/250/500 ms. Not exercised on
  hardware.
- Any non-cancellation Write/Drain error reopens the device, including a non-device
  error; a persistent non-device error would reopen on every utterance (bounded by the
  backoff only when the open itself fails).
- A panic in a command handler keeps the session alive; state touched by that handler
  is assumed consistent.
- Per-request panic in the dispatcher worker does not invalidate the sink (sink panics
  inside ALSA/JACK writers do, via `ErrWriterPanic`).
- Boot prewarm, the P1 items (bare id-less 5xx errors, 225/701 ordering, DEAD engine
  recovery) and PipeWire audio are untouched.
- The soak ran against a null sink: no real-device behaviour was validated.

## Reproduce

Build and rig live in the session scratchpad
(`/tmp/-www-a11y/.../scratchpad/{rig,soak}`); the soak client is about 280
lines of Go and can be moved into `scripts/` if wanted. Commands:
`. ./env.sh; cd refs/voicego; go vet ./... && go test -race ./...` and
`go test -race -tags espeak_ng_real ./cmd/voicego-engine-espeak-ng`.

## Review round (reviewer verdict: merge after fixes)

New commits on the same branch (no rewrite):

| Hash | Subject |
|---|---|
| 48b7f24 | fix(dispatch): honour a STOP that races with request pickup |
| 3f1fca9 | fix(plugin): cap consecutive manager panics |
| 616b0e5 | fix(dispatch): bound sink opens in the cache and reopen after the old close |
| 742576d | fix(server): close the session after a writer panic |
| f4dc9f8 | test(dispatch): make the Synthesize-start test send a real STOP |
| 57f8e73 | docs: drop a private issue id from a sink cache comment |

1. **Blocking, regression from the branch.** A request picked up between the run loop's
   quit check and `process()` registering its cancel func escaped the first STOP, and
   since `cancelClient` had already removed the worker, a second STOP could not reach it.
   `process()` now re-checks `w.quit` inside the `activeMu` section (the reviewer's fix).
   `TestStopRacingWithPickupNeverEscapes`: 1500 rounds of SPEAK, 0-60 us gap, STOP, STOP;
   fails without the fix, 0 escapes with it. `ARCHITECTURE_V2.md` corrected.
2. Plugin manager: three consecutive panicking steps mark the engine DEAD
   (`TestProtectCapsConsecutivePanics`, `TestProtectStreakResets`).
3. Sink cache enforces the open timeout itself (the Open runs in a goroutine, a late
   success is closed). `TestSinkHungOpenTimesOut` uses an Open that ignores its context.
4. A dropped handle's Close runs in the background but the reopen waits for it (max
   500 ms). `TestSinkReopenWaitsForOldHandleClose`.
5. A writer panic poisons the session writer and closes the connection
   (`TestSession_WriterPanicClosesConnection`).
6. The Synthesize-start test now uses a Synthesize that blocks until its context is
   cancelled, sends a real STOP, and returns a gRPC Canceled status; it fails without
   the classification fix.
7. Comments no longer cite private audit item ids (in the diff of this branch).

### Gates at 57f8e73

- `gofmt -l .` empty; `go vet ./...` clean.
- `go test -race -count=1 ./...` run twice: both pass (27 packages ok each time).
- `go test -race -tags espeak_ng_real ./cmd/voicego-engine-espeak-ng ./internal/... ./plugins/...`: pass.
- Latency, null sink, real espeak-ng, n=500 per run, interleaved, load average about 12.
  SPEAK to `701` p50 / p99 (ms): base 1.740/2.352, 1.725/2.531, 1.728/2.449; branch
  1.752/2.520, 1.733/2.486, 1.756/2.493; binary framing base 1.746/2.474, branch
  1.737/2.427. No regression (p50 within 0.03 ms, p99 within 0.17 ms in both directions).
- 10-minute soak, race build, null sink, 6 connections, same traffic mix and checks as before:
  `FINAL queued=117976 end=105887 canceled=12089 accounted=117976 rejected=22665
  stops=6053 | lost=0 mustEnd=0 asyncErr=0 proto=0`, client rc=0, daemon alive,
  no `panic` or `DATA RACE` in the log, no GORACE file, goroutines flat at 47.

## Second review round

Commit `7cf7a1c` fix(dispatch,plugin): do not close a handle whose timed-out Open is still running.

- **Blocking (data race introduced by 616b0e5).** On an Open timeout `runOpen` still
  passed the plugin to `finishAttempt`, which closed it in the background while the
  abandoned Open used the same instance, and the late-result goroutine closed it again.
  Ownership now moves to the late-result goroutine on timeout (`finishAttempt` gets nil).
  Test `TestSinkTimedOutOpenIsNotClosedConcurrently` (Open ignores its context and writes a
  plain field, 50 ms timeout, factory path): reports DATA RACE under `-race` without the
  fix, clean with it, and asserts one Open and exactly one Close.
- **DEAD is terminal.** After three consecutive manager panics the engine is marked DEAD,
  the health watcher is stopped, the plugin process is killed, and a late crash report is
  ignored by the manager loop. The test checks the watcher is stopped.
- **No-factory reuse after a timeout** is documented as test-only in the sink cache.

Gates at 7cf7a1c: `gofmt -l` empty; `go vet ./...` clean; `go test -race -count=1 ./...`
twice, both pass (27 packages ok); `-tags espeak_ng_real` pass; 5-minute race soak
`FINAL queued=61035 end=54709 canceled=6326 accounted=61035 rejected=11700 stops=3130 |
lost=0 mustEnd=0 asyncErr=0 proto=0`, daemon alive, no panic or DATA RACE in the log.
Latency was not re-run (the change is on the open path only).

## Real audio (main at 7cf7a1c, branch fix/real-audio, no code changes)

Result: voicego builds and passes with the real audio backends unchanged. No commits, so
nothing was pushed and the empty local branch was deleted.

- **Build prerequisite.** `libasound2-dev` and `libjack-jackd2-dev` are installed, but
  `libsamplerate0-dev` is not (only the runtime `libsamplerate0`), so `pkg-config samplerate`
  fails and both sink packages refuse to build with their real tags. To proceed without
  sudo I used a build-only shim in the scratchpad: a `samplerate.h` with the 0.2.x
  declarations the code uses (`SRC_DATA`, `src_new/process/reset/delete`, the short/float
  helpers), a `samplerate.pc` and a `libsamplerate.so` symlink to the installed `.so.0`,
  selected with `PKG_CONFIG_PATH`. Nothing was installed or changed system-wide. The
  proper fix is `apt-get install --no-install-recommends libsamplerate0-dev`; a build
  against the real header should be repeated once it is installed.
- **Build and tests.** `go build` and `go vet` with `alsa_real,jack_real,espeak_ng_real`: no
  errors. `go test -race -tags 'alsa_real espeak_ng_real' ./...`: all packages pass.
  `-tags jack_real ./plugins/output/jack2`: pass (build and unit tests only; no JACK
  server, the live tests need `VOICEGO_JACK_REAL=1` and were skipped). The whole-tree
  Makefile `REAL_TAGS` also names `espeak_real`, `rhvoice_real`, `piper_real`; their
  libraries are not installed, so those were not built. ALSA live tests with
  `VOICEGO_ALSA_REAL_DEVICE=default` (all pass): opened the default PCM, achieved
  22050 Hz mono, period 470 frames (21.3 ms), buffer 940 frames (42.6 ms), no backend
  resampling; 3 s of audio written in 2.98 s (real-time pacing).
- **Host setup.** The default PCM is a `plug` over `dmix:CARD=sofhdadsp` from the user's
  `~/.asoundrc`: shared, never an exclusive `hw:` device. Default volume in the test
  config was -90 (wire scale), short utterances. speech-dispatcher and Firefox were not
  touched; no PipeWire restart.
- **Live ALSA latency** (voicegod with alsa sink, real espeak-ng, default PCM, n=30 for
  speak, n=20 for the others): SPEAK to `225` p50 55 us; SPEAK to `701` (first chunk
  written into the ALSA PCM) p50 2.09 ms, p99 2.55 ms (null sink: 1.75 / 2.5). Audible
  start adds the device path: at least the 42.6 ms buffer plus dmix; I could not measure
  that acoustically (no loopback capture). SPEAK to `702` p50 1.0 s for the 0.5-1 s test
  phrase, i.e. drain waits for real playout. STOP to `210` p50 0.13 ms, p99 0.20 ms;
  STOP to `703` p50 10.5 ms, p90 21.4 ms, max 22.2 ms (includes the sink cancel,
  `snd_pcm_drop`); audio already in the dmix/PipeWire path is not included. Pipelined
  STOP then SPEAK: 20/20 completed, 0 cancelled, with and without waiting for the 210.
- **Device loss and reopen**, simulated safely without touching the user's audio: a private
  ALSA config (`ALSA_CONFIG_PATH`, an include of the system `alsa.conf` plus a `file`
  PCM writing to a FIFO) was given to a separate voicegod, and the FIFO reader was killed.
  Result with the real libasound path: the SPEAK in flight got `701` then `502`
  (`snd_pcm_writei: Input/output error`); the log shows "sink failed; will reopen on next
  use". The reopen then blocks in `snd_pcm_open` (the FIFO has no reader), the cache
  abandons it after the 10 s attempt timeout ("open timed out after 10s", retry in
  100 ms), and the abandoned Open's late handle is closed; when the reader came back the
  next SPEAK opened a new sink and 4 of 4 following SPEAKs ended normally in 6-7 ms.
  Observation: while the device is down a SPEAK can wait up to the 10 s open timeout
  before failing; a shorter timeout may suit a screen reader (not changed).
  A real PipeWire/dmix restart was not simulated.
- **Soaks, race detector, 5 minutes each.**
  Real ALSA (default PCM, volume -90, brief utterances, 3 connections, same SPEAK/STOP/
  CANCEL mix): `queued=642 end=427 canceled=215 accounted=642 stops=111 | lost=0 mustEnd=0
  asyncErr=0 proto=0`, daemon alive, no panic or DATA RACE, no GORACE file, goroutines
  41 to 37, RSS 136-138 MB (race build). Throughput is low because audio plays in real
  time.
  Null sink (same real-tag binary, standard heavy mix, 6 connections): `queued=61229
  end=54868 canceled=6361 accounted=61229 stops=3145 | lost=0 mustEnd=0 asyncErr=0
  proto=0`, no panic or DATA RACE.
- **Not covered.** JACK live behaviour (no server); acoustic latency and cancel-to-silence at
  the speakers; a real PipeWire restart; sustained operation beyond 5 minutes.
- **Review.** Ready for review; there is no diff to review, only this test evidence.
