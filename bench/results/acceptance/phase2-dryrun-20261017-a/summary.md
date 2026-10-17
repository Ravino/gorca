# Phase 2 acceptance: DRY RUN, NOT ACCEPTANCE DATA (gates pass)

Mode: **DRY RUN (not acceptance), PARTIAL (m4 x1)**. Run dir: `<repo>/bench/results/acceptance/phase2-dryrun-20261017-a`.
Resolved topology (from the reader reports): lanes-rtx2.

## Steps

| step | result | attempt | wall | load1 min-max | mem PSI max | gated rows | FAIL rows | notes |
|---|---|---|---|---|---|---|---|---|
| m4/e2e | PASS | a1 | 104 s | 13.0-16.2 | 2.510 | 8 | 0 | - |
| x1/hooks | PASS | a1 | 13 s | 11.8-12.7 | 1.010 | 19 | 0 | - |

## Gates

### m4/e2e (attempt 1, load 13.0-16.2, mem PSI some avg10 max 2.510)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| A1.a1_ms.p99 (daemon/voicego) | <= 15 ms | 5.397 | 5.397 | PASS | 13.0-16.2 |
| A1.a1_wrong (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.0-16.2 |
| A1.a1w_ms.p99 (daemon/voicego) | <= 15 ms | 3.628 | 3.628 | PASS | 13.0-16.2 |
| A1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.0-16.2 |
| N1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.0-16.2 |
| N1.n1_focus_ms.p99 (daemon/voicego) | <= 15 ms | 0.348 | 0.348 | PASS | 13.0-16.2 |
| N1.n1_interrupted (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.0-16.2 |
| N1.n1_text_wrong (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.0-16.2 |

### x1/hooks (attempt 1, load 11.8-12.7, mem PSI some avg10 max 1.010)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| X1.native-crash_reset_next_call.p99 | <= 50000 us | 29.118 | 29.118 | PASS | 11.8-12.7 |
| X1.native-on_event_focus.p99 | <= 100 us | 0.074 | 0.074 | PASS | 11.8-12.7 |
| X1.native-on_event_pass.p99 | <= 100 us | 0.080 | 0.080 | PASS | 11.8-12.7 |
| X1.native-on_event_rewrite.p99 | <= 100 us | 0.124 | 0.124 | PASS | 11.8-12.7 |
| X1.native-present_none.p99 | <= 100 us | 0.087 | 0.087 | PASS | 11.8-12.7 |
| X1.native-present_some.p99 | <= 100 us | 0.136 | 0.136 | PASS | 11.8-12.7 |
| X1.native-storm_on_event_1t.p99 | <= 100 us | 0.089 | 0.089 | PASS | 11.8-12.7 |
| X1.native-storm_on_event_4t.p99 | <= 100 us | 0.166 | 0.166 | PASS | 11.8-12.7 |
| X1.native-storm_on_event_8t.p99 | <= 100 us | 0.595 | 0.595 | PASS | 11.8-12.7 |
| X1.wasm-crash_reset_next_call.p99 | <= 50000 us | 113.816 | 113.816 | PASS | 11.8-12.7 |
| X1.wasm-on_event_focus.p99 | <= 100 us | 0.291 | 0.291 | PASS | 11.8-12.7 |
| X1.wasm-on_event_pass.p99 | <= 100 us | 0.353 | 0.353 | PASS | 11.8-12.7 |
| X1.wasm-on_event_rewrite.p99 | <= 100 us | 0.620 | 0.620 | PASS | 11.8-12.7 |
| X1.wasm-present_none.p99 | <= 100 us | 0.357 | 0.357 | PASS | 11.8-12.7 |
| X1.wasm-present_some.p99 | <= 100 us | 0.831 | 0.831 | PASS | 11.8-12.7 |
| X1.wasm-spin_interrupt_reset_next_call.p99 | <= 50000 us | 323.639 | 323.639 | PASS | 11.8-12.7 |
| X1.wasm-storm_on_event_1t.p99 | <= 100 us | 0.337 | 0.337 | PASS | 11.8-12.7 |
| X1.wasm-storm_on_event_4t.p99 | <= 100 us | 0.483 | 0.483 | PASS | 11.8-12.7 |
| X1.wasm-storm_on_event_8t.p99 | <= 100 us | 0.833 | 0.833 | PASS | 11.8-12.7 |

## Host

```
mode=DRY RUN (not acceptance), PARTIAL (m4 x1)
utc=2026-10-16T23:16:25Z
git_rev=c4183c7 dirty_tracked_files=0 branch=worktree-agent-a212e0b0695e3d0d9
kernel=7.1.8+deb13-rt-amd64 cpus=16
loadavg=13.33 10.71 10.16 5/3129 2442354
psi_memory_some_avg10=0.01 psi_memory_full_avg10=0.01
psi_cpu_some_avg10=0.72 psi_io_some_avg10=0.00
mem_available_mb=14642
governor=performance 
rtkit MaxRealtimePriority: i 20 
rtkit RTTimeUSecMax: x 200000 
topology: shipped default (--topology=auto); expected arm: lanes-rt (gated by e2e --expect-topology)
repeats=1 quick=--quick suites=m4 x1
quiet-host:
  governor: performance 
  idle POLL latency=0us disabled=0
  idle C1_ACPI latency=1us disabled=0
  idle C2_ACPI latency=127us disabled=1
  idle C3_ACPI latency=1048us disabled=1
  load: 13.33 10.71 10.16 5/3131 2442394
git_rev_built=c4183c7 binary=e945a7b9567bd5de mtime=2026-10-17 02:18:56.945936891 +0300
loadavg_end=11.33 14.11 12.31 4/3257 2483733 elapsed_s=117
---- resumed ----
mode=DRY RUN (not acceptance), PARTIAL (m4 x1)
utc=2026-10-16T23:47:14Z
git_rev=c4183c7 dirty_tracked_files=1 branch=worktree-agent-a212e0b0695e3d0d9
kernel=7.1.8+deb13-rt-amd64 cpus=16
loadavg=12.56 14.68 15.15 1/2662 2647548
psi_memory_some_avg10=0.41 psi_memory_full_avg10=0.41
psi_cpu_some_avg10=0.07 psi_io_some_avg10=0.00
mem_available_mb=16906
governor=performance 
rtkit MaxRealtimePriority: i 20 
rtkit RTTimeUSecMax: x 200000 
topology: shipped default (--topology=auto); expected arm: lanes-rt (gated by e2e --expect-topology)
repeats=1 quick=--quick suites=m4 x1
quiet-host:
  governor: performance 
  idle POLL latency=0us disabled=0
  idle C1_ACPI latency=1us disabled=0
  idle C2_ACPI latency=127us disabled=1
  idle C3_ACPI latency=1048us disabled=1
  load: 12.56 14.68 15.15 1/2664 2647588
git_rev_built=c4183c7 binary=e945a7b9567bd5de mtime=2026-10-17 02:18:56.945936891 +0300
loadavg_end=12.56 14.68 15.15 1/2662 2647615 elapsed_s=0
```
