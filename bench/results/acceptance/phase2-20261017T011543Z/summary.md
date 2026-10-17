# Phase 2 acceptance: FAIL

Mode: **ACCEPTANCE ATTEMPT, DEGRADED HOST (--force), PARTIAL (m1 m6)**. Run dir: `<repo>/.claude/worktrees/agent-ab1f7350482f9d118/bench/results/acceptance/phase2-20261017T011543Z`.
Resolved topology (from the reader reports): not reported (wrappers without --expect-topology only).

## Steps

| step | result | attempt | wall | load1 min-max | mem PSI max | gated rows | FAIL rows | notes |
|---|---|---|---|---|---|---|---|---|
| m1/e2e | FAIL | a1 | 1 s | 14.5-14.5 | 0 | 0 | 0 | exit 1 |
| m6/fake | FAIL | a1 | 1 s | 15.4-15.4 | 0 | 0 | 0 | exit 1 |
| m6/vv | FAIL | a1 | 6 s | 15.4-15.5 | 0.560 | 0 | 0 | exit 1 |

## Failures

- m1/e2e: exit 1
- m6/fake: exit 1
- m6/vv: exit 1

## Gates

### m1/e2e (attempt 1, load 14.5-14.5, mem PSI some avg10 max 0)

_no gated rows_

### m6/fake (attempt 1, load 15.4-15.4, mem PSI some avg10 max 0)

_no gated rows_

### m6/vv (attempt 1, load 15.4-15.5, mem PSI some avg10 max 0.560)

_no gated rows_

## Host

```
mode=ACCEPTANCE ATTEMPT, DEGRADED HOST (--force), PARTIAL (m1 m6)
utc=2026-10-17T01:15:43Z
git_rev=ce46cac dirty_tracked_files=0 branch=worktree-agent-ab1f7350482f9d118
kernel=7.1.8+deb13-rt-amd64 cpus=16
loadavg=7.91 9.37 9.96 3/2467 3102738
psi_memory_some_avg10=0.09 psi_memory_full_avg10=0.09
psi_cpu_some_avg10=0.02 psi_io_some_avg10=3.17
mem_available_mb=16925
governor=performance 
rtkit MaxRealtimePriority: i 20 
rtkit RTTimeUSecMax: x 200000 
topology: shipped default (--topology=auto); expected arm: lanes-rt (gated by e2e --expect-topology)
repeats=3 quick=no suites=m1 m6
quiet-host:
  governor: performance 
  idle POLL latency=0us disabled=0
  idle C1_ACPI latency=1us disabled=0
  idle C2_ACPI latency=127us disabled=1
  idle C3_ACPI latency=1048us disabled=1
  load: 7.91 9.37 9.96 2/2468 3102778
git_rev_built=d108974 binary=d5edf8b3ee4ce5bd mtime=2026-10-17 04:16:36.049361922 +0300
loadavg_end=15.49 12.10 10.89 7/2544 3116107 elapsed_s=8
```
