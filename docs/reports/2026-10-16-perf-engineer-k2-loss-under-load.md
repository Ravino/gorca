# K2/K2s/R1 "lost" under host load: attribution

Branch `perf/k2-loss-accounting` (from main b8521b1). Conditions: `gorca-hog --threads 16` plus the
other agents on the host: load 30-47 on 16 cores, governor performance, dbus-daemon private
session, `--impersonate-orca`, GORCA_M1/M4 strict, voicego mock, 1200 chords per repeat.

## Reproduction (pre-fix harness)
`bench/results/e2e/k2loss-hog16-base` (20 repeats K2+K2s): K2 lost max 22, K2s lost max 10 (median
0/2). `k2loss-hog16-log` (10 repeats K2, reader stderr kept): one repeat lost=4 with
"1200 spoken for 1200 chords", one lost=1 with "1199 spoken for 1200". All three kept as evidence.

## Attribution (reader shutdown counters, `speech lane stats`, `core stats`)
- Repeat with 1199 spoken: `utterances 1231 = spoken 1230 + superseded 1`. The next chord's answer
  overwrote the previous one in the speech lane's single interrupt slot (`SpeechHandle::submit`,
  `crates/gorca-speech/src/lane.rs`) before the lane thread woke. Latest wins by design (E-2);
  counted, and the newer answer was spoken. Not a bug.
- Repeat with lost=4 and 1200 spoken: `superseded 0`, nothing dropped. The answers reached the
  mock server after the next key was emitted (descheduled reader lane / mock thread). Late, not
  lost; the old window scoring (first SPEAK in [emit_i, emit_i+1)) miscounted them as lost.
- R1 already had text attribution (`r1_late_keys`); its residual "lost" were the same
  superseded slot overwrites (missing_text == superseded in the 20-repeat run).

Verdict: (b)+(c): harness scoring artefact on top of deliberate latest-wins under CPU starvation.
No silent loss found in ingress, command dispatch or lane.

## Fix (harness only)
`crates/gorca-bench/src/e2e/account.rs`: after a K/R series the harness SIGTERMs the reader and
checks conservation from its shutdown counters: `core utterances == lane spoken + superseded +
stale + inbox + muted + submit_failed`, and `lane spoken <= SPEAK verbs the mock received`.
`lost` is now the violation count (a silent loss). New info rows: `superseded`, `late` (K2/K2s/K3),
`missing_text` (R1). Stub readers (no counters) keep the old window scoring.

## Validation
`bench/results/e2e/k2loss-hog16-acct` (K2, K2s, R1 x 20, load 30-47): lost = 0 in all 60 runs;
superseded max 24 (K2) / 13 (K2s) / 12 (R1); late max 4. Latency gates (K2 p99 5.5, R1 p99 6.2 ms)
fail at this load: expected, load is 2-3x the normal range; not a loss.

## Open
- Slot overwrite ignores priority: a newer-epoch utterance of any priority replaces an Interrupt
  in the slot. Harmless for chords; check that a Message/alert can't be overwritten by a typing echo.
- Superseded rate scales with lane-thread scheduling delay; B-5 topology (SCHED_RR via rtkit)
  should reduce it, not measured here.
