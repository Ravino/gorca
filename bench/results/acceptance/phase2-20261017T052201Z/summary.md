# Phase 2 acceptance: FAIL

Mode: **ACCEPTANCE ATTEMPT, DEGRADED HOST (--force), PARTIAL (m6)**. Run dir: `<repo>/bench/results/acceptance/phase2-20261017T052201Z`.
Resolved topology (from the reader reports): lanes-rtx3, lanes-rtx39.

## Steps

| step | result | attempt | wall | load1 min-max | mem PSI max | gated rows | FAIL rows | notes |
|---|---|---|---|---|---|---|---|---|
| m6/fake | FAIL | a1 | 2639 s | 9.7-53.0 | 20.730 | 45 | 4 | exit 1; 4 gate rows FAIL |
| m6/vv | FAIL | a1 | 370 s | 16.8-21.1 | 10.720 | 24 | 4 | exit 1; 4 gate rows FAIL |

## Failures

- m6/fake: exit 1, 4 gate rows FAIL
- m6/vv: exit 1, 4 gate rows FAIL

## Failing gates

- m6/fake: V2.v2_key_ms.p99 median 5.017 max 5.366 ms (gate <= 4)
- m6/fake: V3.v3_prompt_missing median 0 max 1 events (gate <= 0)
- m6/fake: V3.v3_stale median 0 max 1 events (gate <= 0)
- m6/fake: VF.vf_failures median 0 max 1 steps (gate <= 0)
- m6/vv: VV.vv_failures median 0 max 2 steps (gate <= 0)
- m6/vv: VV.vv_flood_key_ms.p99 median 106.527 max 122.088 ms (gate <= 4)
- m6/vv: VV.vv_screen_failures median 0 max 1 steps (gate <= 0)
- m6/vv: VV.vv_type_missing median 0 max 2 events (gate <= 0)

## Gates

### m6/fake (attempt 1, load 9.7-53.0, mem PSI some avg10 max 20.730)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| V2.v2_key_ms.p99 (daemon/voicego) | <= 4 ms | 5.017 | 5.366 | FAIL | 9.7-53.0 |
| V3.v3_prompt_missing (daemon/voicego) | <= 0 events | 0 | 1 | FAIL | 9.7-53.0 |
| V3.v3_stale (daemon/voicego) | <= 0 events | 0 | 1 | FAIL | 9.7-53.0 |
| VF.vf_failures (daemon/voicego) | <= 0 steps | 0 | 1 | FAIL | 9.7-53.0 |
| V1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.7-53.0 |
| V1.v1_ms.p99 (daemon/voicego) | <= 35 ms | 33.026 | 34.286 | PASS | 9.7-53.0 |
| V1.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 9.7-53.0 |
| V2.v2_cat_after_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.7-53.0 |
| V2.v2_key_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.7-53.0 |
| V2.v2_make_after_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.7-53.0 |
| V2.v2_siocinq_measured (daemon/voicego) | >= 1 flag | 1 | 1 | PASS | 9.7-53.0 |
| V2.v2_siocinq_p99_kib (daemon/voicego) | <= 1024 KiB | 0 | 0.203 | PASS | 9.7-53.0 |
| V2.v2_worst_cpu_pct (daemon/voicego) | <= 60 % of one core | 2.128 | 2.238 | PASS | 9.7-53.0 |
| V2.v2_yes_after_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.7-53.0 |
| V2.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 9.7-53.0 |
| V3.v3_no_stop (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.7-53.0 |
| V3.v3_stop_ms.p99 (daemon/voicego) | <= 2 ms | 0.220 | 1.948 | PASS | 9.7-53.0 |
| V3.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 9.7-53.0 |
| VB.vb_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.7-53.0 |
| VB.vb_key_ms.p99 (daemon/voicego) | <= 4 ms | 0.549 | 0.652 | PASS | 9.7-53.0 |
| VB.vb_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.7-53.0 |
| VB.vb_stray_speaks (daemon/voicego) | <= 0 utterances | 0 | 0 | PASS | 9.7-53.0 |
| VB.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 9.7-53.0 |
| VF.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 9.7-53.0 |
| VH.vh_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 9.7-53.0 |
| VH.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 9.7-53.0 |
| VP.vp_echo_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.7-53.0 |
| VP.vp_echo_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.7-53.0 |
| VP.vp_log_lines (daemon/voicego) | >= 1 lines | 34 | 34 | PASS | 9.7-53.0 |
| VP.vp_plaintext_in_logs (daemon/voicego) | <= 0 occurrences | 0 | 0 | PASS | 9.7-53.0 |
| VP.vp_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.7-53.0 |
| VP.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 9.7-53.0 |
| VR.vr_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 9.7-53.0 |
| VR.vr_ipc_calls (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 9.7-53.0 |
| VR.vr_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.7-53.0 |
| VR.vr_ms.p99 (daemon/voicego) | <= 3 ms | 0.678 | 0.688 | PASS | 9.7-53.0 |
| VR.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 9.7-53.0 |
| VT.vt_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.7-53.0 |
| VT.vt_extra (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.7-53.0 |
| VT.vt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.7-53.0 |
| VT.vt_ms.p99 (daemon/voicego) | <= 3 ms | 1.727 | 3.688 | PASS | 9.7-53.0 |
| VT.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 9.7-53.0 |
| VW.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 9.7-53.0 |
| VW.vw_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.7-53.0 |
| VW.vw_ms.p99 (daemon/voicego) | <= 35 ms | 32.210 | 32.345 | PASS | 9.7-53.0 |

### m6/vv (attempt 1, load 16.8-21.1, mem PSI some avg10 max 10.720)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| VV.vv_failures (daemon/voicego) | <= 0 steps | 0 | 2 | FAIL | 16.8-21.1 |
| VV.vv_flood_key_ms.p99 (daemon/voicego) | <= 4 ms | 106.527 | 122.088 | FAIL | 16.8-21.1 |
| VV.vv_screen_failures (daemon/voicego) | <= 0 steps | 0 | 1 | FAIL | 16.8-21.1 |
| VV.vv_type_missing (daemon/voicego) | <= 0 events | 0 | 2 | FAIL | 16.8-21.1 |
| VV.vv_focus_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 16.8-21.1 |
| VV.vv_key_ms.p99 (daemon/voicego) | <= 3 ms | 0.576 | 34.531 | PASS | 16.8-21.1 |
| VV.vv_password_echo_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 16.8-21.1 |
| VV.vv_password_echo_extra (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 16.8-21.1 |
| VV.vv_password_echo_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 16.8-21.1 |
| VV.vv_password_log_empty (daemon/voicego) | <= 0 files | 0 | 0 | PASS | 16.8-21.1 |
| VV.vv_password_plaintext_in_logs (daemon/voicego) | <= 0 occurrences | 0 | 0 | PASS | 16.8-21.1 |
| VV.vv_password_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 16.8-21.1 |
| VV.vv_seq_flood_unspoken (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 16.8-21.1 |
| VV.vv_seq_key_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 16.8-21.1 |
| VV.vv_seq_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 16.8-21.1 |
| VV.vv_server_aborts (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 16.8-21.1 |
| VV.vv_siocinq_p99_kib (daemon/voicego) | <= 1024 KiB | 0 | 0.189 | PASS | 16.8-21.1 |
| VV.vv_stop_ms.p99 (daemon/voicego) | <= 2 ms | 0.245 | 2.636 | PASS | 16.8-21.1 |
| VV.vv_type_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 16.8-21.1 |
| VV.vv_type_extra (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 16.8-21.1 |
| VV.vv_worst_cpu_pct (daemon/voicego) | <= 60 % of one core | 2.465 | 3.223 | PASS | 16.8-21.1 |
| VV.vv_yes_flood_unspoken (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 16.8-21.1 |
| VV.vv_yes_key_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 16.8-21.1 |
| VV.vv_yes_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 16.8-21.1 |

## Host

```
mode=ACCEPTANCE ATTEMPT, DEGRADED HOST (--force), PARTIAL (m6)
utc=2026-10-17T05:22:01Z
git_rev=6a86749 dirty_tracked_files=4 branch=worktree-agent-a442a2fa7ae77a061
kernel=7.1.8+deb13-rt-amd64 cpus=16
loadavg=16.72 17.61 17.66 8/2773 122732
psi_memory_some_avg10=0.01 psi_memory_full_avg10=0.01
psi_cpu_some_avg10=0.84 psi_io_some_avg10=6.02
mem_available_mb=13995
governor=performance 
rtkit MaxRealtimePriority: i 20 
rtkit RTTimeUSecMax: x 200000 
topology: shipped default (--topology=auto); expected arm: lanes-rt (gated by e2e --expect-topology)
repeats=3 quick=no suites=m6
quiet-host:
  governor: performance 
  idle POLL latency=0us disabled=0
  idle C1_ACPI latency=1us disabled=0
  idle C2_ACPI latency=127us disabled=1
  idle C3_ACPI latency=1048us disabled=1
  load: 16.72 17.61 17.66 2/2773 122774
git_rev_built=6a86749 binary=1b2536e2803bfd10 mtime=2026-10-17 08:21:56.416444451 +0300
loadavg_end=18.22 18.20 18.29 2/2701 513276 elapsed_s=3009
```
