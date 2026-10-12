# Direct espeak-ng (libespeak-ng 1.52.0) as a gorca backend

Installed: `libespeak-ng1`, `libespeak-ng-dev` 1.52.0+dfsg-5 (`#include <espeak-ng/speak_lib.h>`,
`-lespeak-ng`). Sources: `refs/espeak-ng/src/include/espeak-ng/speak_lib.h`, speechd's use in
`refs/speech-dispatcher/src/modules/espeak.c`.

## API used
* `espeak_Initialize(AUDIO_OUTPUT_SYNCHRONOUS, buflength_ms, data_path, options)` returns the sample
  rate (22050 Hz live). Synchronous mode delivers PCM through the callback
  `int cb(short *wav, int numsamples, espeak_EVENT *events)`; return 1 from the callback to abort the
  current synthesis (this is the cancel primitive, granularity = one callback = `buflength` ms; 0 ->
  default 60 ms per the header; speechd passes 300). `AUDIO_OUTPUT_PLAYBACK` makes the library play (PortAudio/pulse) on its own
  thread; avoid it, gorca should own the audio stream (PipeWire).
* `espeak_SetSynthCallback`, `espeak_SetVoiceByName("en-us")`, `espeak_SetParameter(espeakRATE|PITCH|VOLUME|PUNCTUATION|CAPITALS, v, relative)`,
  `espeak_Synth(text, size, 0, POS_CHARACTER, 0, espeakCHARS_AUTO|espeakSSML, NULL, user)`,
  `espeak_Synchronize()`, `espeak_Cancel()`, `espeak_Terminate()`.
* The library has global state and is not thread-safe: one synthesis thread, requests serialised.
* Rate: wpm, library default 175; speechd's espeak-ng config normal 170, max 449.

## Measurements (`tools/platform-probes/espeak_lat.c`, `espeak_cold.c`, 2026-10-12)
Synchronous mode, callback timestamps via `CLOCK_MONOTONIC`, en-us, no audio device involved.

| Metric | Result |
|---|---|
| `espeak_Initialize` | 0.5-3.3 ms (cold process) |
| `espeak_SetVoiceByName("en-us")` first call | 2.3-2.8 ms |
| first `espeak_Synth("Hello")` -> first PCM callback, cold process | 0.05-0.08 ms |
| warm, 1 char / 11 chars / 80 chars: Synth -> first PCM callback (30 runs each, rates 175/350/500) | 0.01-0.05 ms / 0.01-0.03 ms / 0.08-0.12 ms (max 0.31 ms) |
| total synth time for 80 chars = 4.5 s of audio at 175 wpm | 2.4 ms (~1900x real time); 4.2 ms at 500 wpm |
| switching voice en-us -> ru | 5.8-24.9 ms (cold 25 ms, then ~6 ms) |

So espeak-ng itself contributes well under 1 ms to time-to-first-audio-sample; first audio at the
sink is dominated by the audio stack (PipeWire quantum 21.3 ms default here; a dedicated low-latency
stream with a 128-256 frame buffer is ~3-6 ms) and by how fast gorca cancels the previous stream.
Voice/language switches cost ~6 ms and must be done off the hot path or pre-warmed.

## SSIP vs direct

| | SSIP via speech-dispatcher | Direct libespeak-ng |
|---|---|---|
| Added latency (cmd -> first synth) | ~0.7 ms p50, 3.3 ms max (idle) | ~0.05-0.3 ms |
| Cancel + replace | ~13 ms p50, 20 ms max to new BEGIN, `CANCELED` event 3.6 ms p50 | callback abort within one chunk (<= buflength, 60 ms default) + flush own stream: sub-ms to chunk; replace immediately |
| Priorities / queueing | built in (5 levels) | implement in gorca |
| Multiple synths (Piper, Festival, Speech Dispatcher modules), user's voice config, sound icons, other clients | yes | no (gorca must integrate each engine) |
| Crash isolation | engine in separate process | engine in-process; put it in its own thread/process to honour crash isolation |
| Audio routing | module -> pulse/pipewire/alsa | gorca-owned PipeWire stream, exact flush control |

Recommendation: keep an abstract speech backend. Direct espeak-ng (in a helper process or dedicated
thread) for the zero-latency echo path (keys, characters, short words); SSIP for general utterances,
user-chosen synthesizers and compatibility.
