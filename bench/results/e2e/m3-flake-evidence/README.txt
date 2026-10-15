m3-flake investigation, 2026-10-14. See docs/reports/m3-e2-flake.md.
m3-flake-{old,new}-N : INVALID, run without ",--impersonate-orca" (no SetKeyGrabs); kept, not used.
m3-flakeB-{old,new}-N : interleaved, normal load (4-12), 1 repeat (seed A) each, 7 per arm. old=54a6fef reader, new=4f4a479 reader, same (HEAD) harness.
m3-flakeS-{old,new,instr}-N : 12 CPU hogs (yes>/dev/null, pids in hogs.pids), load 21-25, 2 repeats (seeds A,B) each. instr = 4f4a479 + eprintln/file logging in lane.rs (not committed).
instr-submit-stale.log : INSTR submit lines for every utterance in m3-flakeS-instr-2 (both repeats), plus submit_stale lines.
