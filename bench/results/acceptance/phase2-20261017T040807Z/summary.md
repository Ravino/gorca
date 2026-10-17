# Phase 2 acceptance: FAIL

Mode: **ACCEPTANCE ATTEMPT, DEGRADED HOST (--force), PARTIAL (m1 m6)**. Run dir: `<repo>/.claude/worktrees/agent-a442a2fa7ae77a061/bench/results/acceptance/phase2-20261017T040807Z`.
Resolved topology (from the reader reports): lanes-rtx24, lanes-rtx3, lanes-rtx38,singlex1.

## Steps

| step | result | attempt | wall | load1 min-max | mem PSI max | gated rows | FAIL rows | notes |
|---|---|---|---|---|---|---|---|---|
| m1/e2e | PASS | a1 | 858 s | 9.3-20.2 | 2.690 | 31 | 0 | - |
| m6/fake | FAIL | a1 | 2632 s | 7.5-30.0 | 13.670 | 45 | 5 | exit 3; 5 gate rows FAIL |
| m6/vv | FAIL | a1 | 370 s | 13.5-26.8 | 2.720 | 24 | 1 | exit 1; 1 gate rows FAIL |

## Failures

- m6/fake: exit 3, 5 gate rows FAIL
- m6/vv: exit 1, 1 gate rows FAIL

## Failing gates

- m6/fake: V1.v1_ms.p99 median 39.889 max 40.189 ms (gate <= 35)
- m6/fake: VH.vh_failures median 1 max 1 steps (gate <= 0)
- m6/fake: VT.vt_double median 0 max 2 events (gate <= 0)
- m6/fake: VT.vt_extra median 0 max 9 events (gate <= 0)
- m6/fake: VT.vt_missing median 0 max 37 events (gate <= 0)
- m6/vv: VV.vv_flood_key_ms.p99 median 7.625 max 13.658 ms (gate <= 4)

## Gates

### m1/e2e (attempt 1, load 9.3-20.2, mem PSI some avg10 max 2.690)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| K2.k2_ms.p99 (daemon/voicego) | <= 5 ms | 0.248 | 0.589 | PASS | 9.3-20.2 |
| K2.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.3-20.2 |
| K2.not_consumed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.3-20.2 |
| K2s.k2s_ms.p99 (daemon/voicego) | <= 6 ms | 0.285 | 0.295 | PASS | 9.3-20.2 |
| K2s.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.3-20.2 |
| K2s.not_consumed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.3-20.2 |
| K2s.storm_achieved_eps (daemon/voicego) | >= 45000 ev/s | 49999.598 | 49999.784 | PASS | 9.3-20.2 |
| K3.k3_ms.p99 (daemon/voicego) | <= 15 ms | 1.212 | 2.474 | PASS | 9.3-20.2 |
| K3.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.3-20.2 |
| K3.not_consumed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.3-20.2 |
| K4.app_doubled (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 9.3-20.2 |
| K4.app_lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 9.3-20.2 |
| K4.chord_leaked (daemon/voicego) | <= 0 chords | 0 | 0 | PASS | 9.3-20.2 |
| K4.chord_resent_per_10k (daemon/voicego) | <= 5 chords | 0 | 0 | PASS | 9.3-20.2 |
| K4.flap_failed (daemon/voicego) | <= 0 runs | 0 | 0 | PASS | 9.3-20.2 |
| K4.forged_accepted (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 9.3-20.2 |
| K4.restore_failed (daemon/voicego) | <= 0 runs | 0 | 0 | PASS | 9.3-20.2 |
| K4.wait_timeouts (daemon/voicego) | <= 0 waits | 0 | 0 | PASS | 9.3-20.2 |
| K4h.hold_not_stale (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.3-20.2 |
| K4h.stuck_echoed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.3-20.2 |
| K4h.stuck_hint_wrong (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.3-20.2 |
| K4h.stuck_not_healed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.3-20.2 |
| K4h.stuck_not_swallowed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.3-20.2 |
| R1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.3-20.2 |
| R1.not_consumed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.3-20.2 |
| R1.r1_ms.p99 (daemon/voicego) | <= 3 ms | 0.351 | 1.295 | PASS | 9.3-20.2 |
| R1s.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.3-20.2 |
| R1s.not_consumed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.3-20.2 |
| R1s.r1s_ms.p99 (daemon/voicego) | <= 4 ms | 0.371 | 2.076 | PASS | 9.3-20.2 |
| R1s.storm_achieved_eps (daemon/voicego) | >= 45000 ev/s | 49999.598 | 49999.821 | PASS | 9.3-20.2 |
| RS.rs_failures (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 9.3-20.2 |

### m6/fake (attempt 1, load 7.5-30.0, mem PSI some avg10 max 13.670)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| V1.v1_ms.p99 (daemon/voicego) | <= 35 ms | 39.889 | 40.189 | FAIL | 7.5-30.0 |
| VH.vh_failures (daemon/voicego) | <= 0 steps | 1 | 1 | FAIL | 7.5-30.0 |
| VT.vt_double (daemon/voicego) | <= 0 events | 0 | 2 | FAIL | 7.5-30.0 |
| VT.vt_extra (daemon/voicego) | <= 0 events | 0 | 9 | FAIL | 7.5-30.0 |
| VT.vt_missing (daemon/voicego) | <= 0 events | 0 | 37 | FAIL | 7.5-30.0 |
| V1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 7.5-30.0 |
| V1.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 7.5-30.0 |
| V2.v2_cat_after_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 7.5-30.0 |
| V2.v2_key_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 7.5-30.0 |
| V2.v2_key_ms.p99 (daemon/voicego) | <= 4 ms | 0.613 | 0.766 | PASS | 7.5-30.0 |
| V2.v2_make_after_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 7.5-30.0 |
| V2.v2_siocinq_measured (daemon/voicego) | >= 1 flag | 1 | 1 | PASS | 7.5-30.0 |
| V2.v2_siocinq_p99_kib (daemon/voicego) | <= 1024 KiB | 0 | 0 | PASS | 7.5-30.0 |
| V2.v2_worst_cpu_pct (daemon/voicego) | <= 60 % of one core | 2.204 | 2.224 | PASS | 7.5-30.0 |
| V2.v2_yes_after_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 7.5-30.0 |
| V2.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 7.5-30.0 |
| V3.v3_no_stop (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 7.5-30.0 |
| V3.v3_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 7.5-30.0 |
| V3.v3_stale (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 7.5-30.0 |
| V3.v3_stop_ms.p99 (daemon/voicego) | <= 2 ms | 0.191 | 0.252 | PASS | 7.5-30.0 |
| V3.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 7.5-30.0 |
| VB.vb_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 7.5-30.0 |
| VB.vb_key_ms.p99 (daemon/voicego) | <= 4 ms | 1.938 | 2.155 | PASS | 7.5-30.0 |
| VB.vb_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 7.5-30.0 |
| VB.vb_stray_speaks (daemon/voicego) | <= 0 utterances | 0 | 0 | PASS | 7.5-30.0 |
| VB.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 7.5-30.0 |
| VF.vf_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 7.5-30.0 |
| VF.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 7.5-30.0 |
| VH.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 7.5-30.0 |
| VP.vp_echo_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 7.5-30.0 |
| VP.vp_echo_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 7.5-30.0 |
| VP.vp_log_lines (daemon/voicego) | >= 1 lines | 34 | 34 | PASS | 7.5-30.0 |
| VP.vp_plaintext_in_logs (daemon/voicego) | <= 0 occurrences | 0 | 0 | PASS | 7.5-30.0 |
| VP.vp_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 7.5-30.0 |
| VP.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 7.5-30.0 |
| VR.vr_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 7.5-30.0 |
| VR.vr_ipc_calls (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 7.5-30.0 |
| VR.vr_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 7.5-30.0 |
| VR.vr_ms.p99 (daemon/voicego) | <= 3 ms | 0.392 | 0.985 | PASS | 7.5-30.0 |
| VR.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 7.5-30.0 |
| VT.vt_ms.p99 (daemon/voicego) | <= 3 ms | 0.794 | 3.255 | PASS | 7.5-30.0 |
| VT.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 7.5-30.0 |
| VW.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 7.5-30.0 |
| VW.vw_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 7.5-30.0 |
| VW.vw_ms.p99 (daemon/voicego) | <= 35 ms | 33.045 | 33.139 | PASS | 7.5-30.0 |

### m6/vv (attempt 1, load 13.5-26.8, mem PSI some avg10 max 2.720)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| VV.vv_flood_key_ms.p99 (daemon/voicego) | <= 4 ms | 7.625 | 13.658 | FAIL | 13.5-26.8 |
| VV.vv_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 13.5-26.8 |
| VV.vv_focus_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 13.5-26.8 |
| VV.vv_key_ms.p99 (daemon/voicego) | <= 3 ms | 0.538 | 0.582 | PASS | 13.5-26.8 |
| VV.vv_password_echo_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.5-26.8 |
| VV.vv_password_echo_extra (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.5-26.8 |
| VV.vv_password_echo_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.5-26.8 |
| VV.vv_password_log_empty (daemon/voicego) | <= 0 files | 0 | 0 | PASS | 13.5-26.8 |
| VV.vv_password_plaintext_in_logs (daemon/voicego) | <= 0 occurrences | 0 | 0 | PASS | 13.5-26.8 |
| VV.vv_password_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.5-26.8 |
| VV.vv_screen_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 13.5-26.8 |
| VV.vv_seq_flood_unspoken (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.5-26.8 |
| VV.vv_seq_key_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.5-26.8 |
| VV.vv_seq_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.5-26.8 |
| VV.vv_server_aborts (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.5-26.8 |
| VV.vv_siocinq_p99_kib (daemon/voicego) | <= 1024 KiB | 0 | 0 | PASS | 13.5-26.8 |
| VV.vv_stop_ms.p99 (daemon/voicego) | <= 2 ms | 0.209 | 1.574 | PASS | 13.5-26.8 |
| VV.vv_type_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.5-26.8 |
| VV.vv_type_extra (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.5-26.8 |
| VV.vv_type_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.5-26.8 |
| VV.vv_worst_cpu_pct (daemon/voicego) | <= 60 % of one core | 2.510 | 2.857 | PASS | 13.5-26.8 |
| VV.vv_yes_flood_unspoken (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.5-26.8 |
| VV.vv_yes_key_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.5-26.8 |
| VV.vv_yes_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.5-26.8 |

## Host

```
mode=ACCEPTANCE ATTEMPT, DEGRADED HOST (--force), PARTIAL (m1 m6)
utc=2026-10-17T04:08:07Z
git_rev=6a86749 dirty_tracked_files=4 branch=worktree-agent-a442a2fa7ae77a061
kernel=7.1.8+deb13-rt-amd64 cpus=16
loadavg=16.64 17.28 16.94 3/2941 3842284
psi_memory_some_avg10=1.55 psi_memory_full_avg10=1.55
psi_cpu_some_avg10=9.05 psi_io_some_avg10=0.00
mem_available_mb=16277
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
  load: 16.64 17.28 16.94 1/2924 3842326
git_rev_built=6a86749 binary=8366df2e75cb2401 mtime=2026-10-17 07:07:41.847904103 +0300
loadavg_end=26.84 20.35 18.44 26/2202 66831 elapsed_s=3860
```
