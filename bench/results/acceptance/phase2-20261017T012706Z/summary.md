# Phase 2 acceptance: PASS ON A DEGRADED HOST, NOT ACCEPTANCE

Mode: **ACCEPTANCE ATTEMPT, DEGRADED HOST (--force), PARTIAL (m1 m6)**. Run dir: `<repo>/.claude/worktrees/agent-ab1f7350482f9d118/bench/results/acceptance/phase2-20261017T012706Z`.
Resolved topology (from the reader reports): lanes-rtx24, lanes-rtx3, lanes-rtx39.

## Steps

| step | result | attempt | wall | load1 min-max | mem PSI max | gated rows | FAIL rows | notes |
|---|---|---|---|---|---|---|---|---|
| m1/e2e | PASS | a1 | 836 s | 6.5-14.4 | 2.750 | 31 | 0 | - |
| m6/fake | PASS | a1 | 2609 s | 4.5-10.7 | 3.480 | 45 | 0 | - |
| m6/vv | PASS | a1 | 368 s | 4.4-10.2 | 0.370 | 24 | 0 | - |

## Gates

### m1/e2e (attempt 1, load 6.5-14.4, mem PSI some avg10 max 2.750)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| K2.k2_ms.p99 (daemon/voicego) | <= 5 ms | 0.242 | 0.246 | PASS | 6.5-14.4 |
| K2.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 6.5-14.4 |
| K2.not_consumed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 6.5-14.4 |
| K2s.k2s_ms.p99 (daemon/voicego) | <= 6 ms | 0.242 | 0.350 | PASS | 6.5-14.4 |
| K2s.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 6.5-14.4 |
| K2s.not_consumed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 6.5-14.4 |
| K2s.storm_achieved_eps (daemon/voicego) | >= 45000 ev/s | 49999.654 | 49999.674 | PASS | 6.5-14.4 |
| K3.k3_ms.p99 (daemon/voicego) | <= 15 ms | 1.107 | 1.140 | PASS | 6.5-14.4 |
| K3.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 6.5-14.4 |
| K3.not_consumed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 6.5-14.4 |
| K4.app_doubled (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 6.5-14.4 |
| K4.app_lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 6.5-14.4 |
| K4.chord_leaked (daemon/voicego) | <= 0 chords | 0 | 0 | PASS | 6.5-14.4 |
| K4.chord_resent_per_10k (daemon/voicego) | <= 5 chords | 0 | 1 | PASS | 6.5-14.4 |
| K4.flap_failed (daemon/voicego) | <= 0 runs | 0 | 0 | PASS | 6.5-14.4 |
| K4.forged_accepted (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 6.5-14.4 |
| K4.restore_failed (daemon/voicego) | <= 0 runs | 0 | 0 | PASS | 6.5-14.4 |
| K4.wait_timeouts (daemon/voicego) | <= 0 waits | 0 | 0 | PASS | 6.5-14.4 |
| K4h.hold_not_stale (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 6.5-14.4 |
| K4h.stuck_echoed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 6.5-14.4 |
| K4h.stuck_hint_wrong (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 6.5-14.4 |
| K4h.stuck_not_healed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 6.5-14.4 |
| K4h.stuck_not_swallowed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 6.5-14.4 |
| R1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 6.5-14.4 |
| R1.not_consumed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 6.5-14.4 |
| R1.r1_ms.p99 (daemon/voicego) | <= 3 ms | 0.350 | 0.358 | PASS | 6.5-14.4 |
| R1s.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 6.5-14.4 |
| R1s.not_consumed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 6.5-14.4 |
| R1s.r1s_ms.p99 (daemon/voicego) | <= 4 ms | 0.344 | 0.375 | PASS | 6.5-14.4 |
| R1s.storm_achieved_eps (daemon/voicego) | >= 45000 ev/s | 49999.541 | 49999.662 | PASS | 6.5-14.4 |
| RS.rs_failures (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 6.5-14.4 |

### m6/fake (attempt 1, load 4.5-10.7, mem PSI some avg10 max 3.480)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| V1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.5-10.7 |
| V1.v1_ms.p99 (daemon/voicego) | <= 35 ms | 32.539 | 32.741 | PASS | 4.5-10.7 |
| V1.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 4.5-10.7 |
| V2.v2_cat_after_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.5-10.7 |
| V2.v2_key_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.5-10.7 |
| V2.v2_key_ms.p99 (daemon/voicego) | <= 4 ms | 0.491 | 0.509 | PASS | 4.5-10.7 |
| V2.v2_make_after_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.5-10.7 |
| V2.v2_siocinq_measured (daemon/voicego) | >= 1 flag | 1 | 1 | PASS | 4.5-10.7 |
| V2.v2_siocinq_p99_kib (daemon/voicego) | <= 1024 KiB | 0 | 0 | PASS | 4.5-10.7 |
| V2.v2_worst_cpu_pct (daemon/voicego) | <= 60 % of one core | 1.775 | 1.855 | PASS | 4.5-10.7 |
| V2.v2_yes_after_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.5-10.7 |
| V2.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 4.5-10.7 |
| V3.v3_no_stop (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.5-10.7 |
| V3.v3_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.5-10.7 |
| V3.v3_stale (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.5-10.7 |
| V3.v3_stop_ms.p99 (daemon/voicego) | <= 2 ms | 0.199 | 0.206 | PASS | 4.5-10.7 |
| V3.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 4.5-10.7 |
| VB.vb_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.5-10.7 |
| VB.vb_key_ms.p99 (daemon/voicego) | <= 4 ms | 0.412 | 0.419 | PASS | 4.5-10.7 |
| VB.vb_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.5-10.7 |
| VB.vb_stray_speaks (daemon/voicego) | <= 0 utterances | 0 | 0 | PASS | 4.5-10.7 |
| VB.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 4.5-10.7 |
| VF.vf_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 4.5-10.7 |
| VF.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 4.5-10.7 |
| VH.vh_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 4.5-10.7 |
| VH.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 4.5-10.7 |
| VP.vp_echo_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.5-10.7 |
| VP.vp_echo_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.5-10.7 |
| VP.vp_log_lines (daemon/voicego) | >= 1 lines | 34 | 34 | PASS | 4.5-10.7 |
| VP.vp_plaintext_in_logs (daemon/voicego) | <= 0 occurrences | 0 | 0 | PASS | 4.5-10.7 |
| VP.vp_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.5-10.7 |
| VP.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 4.5-10.7 |
| VR.vr_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 4.5-10.7 |
| VR.vr_ipc_calls (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 4.5-10.7 |
| VR.vr_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.5-10.7 |
| VR.vr_ms.p99 (daemon/voicego) | <= 3 ms | 0.277 | 0.371 | PASS | 4.5-10.7 |
| VR.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 4.5-10.7 |
| VT.vt_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.5-10.7 |
| VT.vt_extra (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.5-10.7 |
| VT.vt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.5-10.7 |
| VT.vt_ms.p99 (daemon/voicego) | <= 3 ms | 0.516 | 0.520 | PASS | 4.5-10.7 |
| VT.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 4.5-10.7 |
| VW.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 4.5-10.7 |
| VW.vw_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.5-10.7 |
| VW.vw_ms.p99 (daemon/voicego) | <= 35 ms | 32.107 | 32.144 | PASS | 4.5-10.7 |

### m6/vv (attempt 1, load 4.4-10.2, mem PSI some avg10 max 0.370)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| VV.vv_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 4.4-10.2 |
| VV.vv_flood_key_ms.p99 (daemon/voicego) | <= 4 ms | 0.558 | 0.662 | PASS | 4.4-10.2 |
| VV.vv_focus_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 4.4-10.2 |
| VV.vv_key_ms.p99 (daemon/voicego) | <= 3 ms | 0.485 | 0.520 | PASS | 4.4-10.2 |
| VV.vv_password_echo_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.4-10.2 |
| VV.vv_password_echo_extra (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.4-10.2 |
| VV.vv_password_echo_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.4-10.2 |
| VV.vv_password_log_empty (daemon/voicego) | <= 0 files | 0 | 0 | PASS | 4.4-10.2 |
| VV.vv_password_plaintext_in_logs (daemon/voicego) | <= 0 occurrences | 0 | 0 | PASS | 4.4-10.2 |
| VV.vv_password_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.4-10.2 |
| VV.vv_screen_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 4.4-10.2 |
| VV.vv_seq_flood_unspoken (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.4-10.2 |
| VV.vv_seq_key_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.4-10.2 |
| VV.vv_seq_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.4-10.2 |
| VV.vv_server_aborts (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.4-10.2 |
| VV.vv_siocinq_p99_kib (daemon/voicego) | <= 1024 KiB | 0 | 0 | PASS | 4.4-10.2 |
| VV.vv_stop_ms.p99 (daemon/voicego) | <= 2 ms | 0.193 | 0.204 | PASS | 4.4-10.2 |
| VV.vv_type_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.4-10.2 |
| VV.vv_type_extra (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.4-10.2 |
| VV.vv_type_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.4-10.2 |
| VV.vv_worst_cpu_pct (daemon/voicego) | <= 60 % of one core | 1.805 | 1.822 | PASS | 4.4-10.2 |
| VV.vv_yes_flood_unspoken (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.4-10.2 |
| VV.vv_yes_key_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.4-10.2 |
| VV.vv_yes_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.4-10.2 |

## Host

```
mode=ACCEPTANCE ATTEMPT, DEGRADED HOST (--force), PARTIAL (m1 m6)
utc=2026-10-17T01:27:06Z
git_rev=d108974 dirty_tracked_files=3 branch=worktree-agent-ab1f7350482f9d118
kernel=7.1.8+deb13-rt-amd64 cpus=16
loadavg=13.61 10.67 10.28 4/2356 3147415
psi_memory_some_avg10=0.00 psi_memory_full_avg10=0.00
psi_cpu_some_avg10=0.09 psi_io_some_avg10=0.00
mem_available_mb=15852
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
  load: 13.61 10.67 10.28 4/2360 3147458
git_rev_built=d108974 binary=69fb92e724e51dbc mtime=2026-10-17 04:27:06.154842875 +0300
loadavg_end=8.71 8.23 8.09 1/2528 3283991 elapsed_s=3813
```
