m3-flake follow-up, 2026-10-14. See docs/reports/m3-e2-flake.md (follow-up section).
m3-bugA-{1,2,3}: old harness (no paste pacing), 12 CPU hogs (load 20-27), 2 repeats, daemon = 9d44caa + "core" stats log line only. stressA-N.out = daemon stderr
  (core stats, speech lane stats, reader text counters). Shows submitted (core utterances) vs spoken+counted: the lane lost utterances silently (state-machine Discard).
m3-e2f-{base,fix}-1: E2f first-key-after-focus probe, normal load; base = daemon without fixes, fix = with fixes.
m3-fin-suite: M3 gates C1,C1h,C2,E1,E2f,S2,SA1,SA2,SA3,SA3a,PW, 3 repeats, normal load, fixed daemon.
m3-fin-e2-{fix,base}-N: E2 x1 repeat each, interleaved fix/base, normal load (3-5), new harness (paste pacing). 0 misses in all 10.
m3-fin-hog-e2-{fix,base}-N: E2 x2 repeats, 12 hogs (load 20-24). m3-fin-hog-e2f-*: E2f under hogs.
final.log: wall time and loadavg at the start of every run. final-*.out: daemon stderr per run. *.sh: the scripts used.
