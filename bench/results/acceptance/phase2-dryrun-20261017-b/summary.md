# Phase 2 acceptance: DRY RUN, NOT ACCEPTANCE DATA (gates FAIL)

Mode: **DRY RUN (not acceptance), PARTIAL (m1 m6 m7)**. Run dir: `<repo>/bench/results/acceptance/phase2-dryrun-20261017-b`.
Resolved topology (from the reader reports): lanes-rtx1, lanes-rtx13, lanes-rtx26, lanes-rtx8.

## Steps

| step | result | attempt | wall | load1 min-max | mem PSI max | gated rows | FAIL rows | notes |
|---|---|---|---|---|---|---|---|---|
| m1/e2e | PASS | a1 | 44 s | 14.7-17.5 | 0.190 | 31 | 0 | - |
| m6/fake | FAIL | a1 | 368 s | 11.4-32.7 | 9.200 | 45 | 2 | exit 1; 2 gate rows FAIL |
| m6/vv | PASS | a1 | 99 s | 13.7-22.4 | 2.750 | 24 | 0 | - |
| m7/firefox | FAIL | a1 | 592 s | 11.0-26.2 | 6.080 | 77 | 6 | exit 1; 6 gate rows FAIL |

## Failures

- m6/fake: exit 1, 2 gate rows FAIL
- m7/firefox: exit 1, 6 gate rows FAIL

## Failing gates

- m6/fake: VP.vp_echo_missing median 1 max 1 events (gate <= 0)
- m6/fake: VR.vr_ms.p99 median 5.245 max 5.245 ms (gate <= 3)
- m7/firefox: FC1.fc1_ms.p99 median 261.163 max 261.163 ms (gate <= 15)
- m7/firefox: FC1.fc_double median 2 max 2 keys (gate <= 0)
- m7/firefox: FC1.fc_late median 77 max 77 speeches (gate <= 0)
- m7/firefox: FC1.lost median 2 max 2 keys (gate <= 0)
- m7/firefox: FC1h.page_dropped median 3 max 3 keys (gate <= 2)
- m7/firefox: FE1.fe1_ms.p99 median 3.854 max 3.854 ms (gate <= 3)

## Gates

### m1/e2e (attempt 1, load 14.7-17.5, mem PSI some avg10 max 0.190)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| K2.k2_ms.p99 (daemon/voicego) | <= 5 ms | 0.294 | 0.294 | PASS | 14.7-17.5 |
| K2.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 14.7-17.5 |
| K2.not_consumed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 14.7-17.5 |
| K2s.k2s_ms.p99 (daemon/voicego) | <= 6 ms | 0.239 | 0.239 | PASS | 14.7-17.5 |
| K2s.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 14.7-17.5 |
| K2s.not_consumed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 14.7-17.5 |
| K2s.storm_achieved_eps (daemon/voicego) | >= 45000 ev/s | 49997.572 | 49997.572 | PASS | 14.7-17.5 |
| K3.k3_ms.p99 (daemon/voicego) | <= 15 ms | 1.054 | 1.054 | PASS | 14.7-17.5 |
| K3.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 14.7-17.5 |
| K3.not_consumed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 14.7-17.5 |
| K4.app_doubled (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 14.7-17.5 |
| K4.app_lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 14.7-17.5 |
| K4.chord_leaked (daemon/voicego) | <= 0 chords | 0 | 0 | PASS | 14.7-17.5 |
| K4.chord_resent_per_10k (daemon/voicego) | <= 5 chords | 0 | 0 | PASS | 14.7-17.5 |
| K4.flap_failed (daemon/voicego) | <= 0 runs | 0 | 0 | PASS | 14.7-17.5 |
| K4.forged_accepted (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 14.7-17.5 |
| K4.restore_failed (daemon/voicego) | <= 0 runs | 0 | 0 | PASS | 14.7-17.5 |
| K4.wait_timeouts (daemon/voicego) | <= 0 waits | 0 | 0 | PASS | 14.7-17.5 |
| K4h.hold_not_stale (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 14.7-17.5 |
| K4h.stuck_echoed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 14.7-17.5 |
| K4h.stuck_hint_wrong (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 14.7-17.5 |
| K4h.stuck_not_healed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 14.7-17.5 |
| K4h.stuck_not_swallowed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 14.7-17.5 |
| R1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 14.7-17.5 |
| R1.not_consumed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 14.7-17.5 |
| R1.r1_ms.p99 (daemon/voicego) | <= 3 ms | 0.283 | 0.283 | PASS | 14.7-17.5 |
| R1s.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 14.7-17.5 |
| R1s.not_consumed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 14.7-17.5 |
| R1s.r1s_ms.p99 (daemon/voicego) | <= 4 ms | 0.476 | 0.476 | PASS | 14.7-17.5 |
| R1s.storm_achieved_eps (daemon/voicego) | >= 45000 ev/s | 49999.893 | 49999.893 | PASS | 14.7-17.5 |
| RS.rs_failures (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 14.7-17.5 |

### m6/fake (attempt 1, load 11.4-32.7, mem PSI some avg10 max 9.200)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| VP.vp_echo_missing (daemon/voicego) | <= 0 events | 1 | 1 | FAIL | 11.4-32.7 |
| VR.vr_ms.p99 (daemon/voicego) | <= 3 ms | 5.245 | 5.245 | FAIL | 11.4-32.7 |
| V1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 11.4-32.7 |
| V1.v1_ms.p99 (daemon/voicego) | <= 35 ms | 32.774 | 32.774 | PASS | 11.4-32.7 |
| V1.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 11.4-32.7 |
| V2.v2_cat_after_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 11.4-32.7 |
| V2.v2_key_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 11.4-32.7 |
| V2.v2_key_ms.p99 (daemon/voicego) | <= 4 ms | 2.924 | 2.924 | PASS | 11.4-32.7 |
| V2.v2_make_after_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 11.4-32.7 |
| V2.v2_siocinq_measured (daemon/voicego) | >= 1 flag | 1 | 1 | PASS | 11.4-32.7 |
| V2.v2_siocinq_p99_kib (daemon/voicego) | <= 1024 KiB | 0 | 0 | PASS | 11.4-32.7 |
| V2.v2_worst_cpu_pct (daemon/voicego) | <= 60 % of one core | 2.095 | 2.095 | PASS | 11.4-32.7 |
| V2.v2_yes_after_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 11.4-32.7 |
| V2.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 11.4-32.7 |
| V3.v3_no_stop (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 11.4-32.7 |
| V3.v3_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 11.4-32.7 |
| V3.v3_stale (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 11.4-32.7 |
| V3.v3_stop_ms.p99 (daemon/voicego) | <= 2 ms | 0.980 | 0.980 | PASS | 11.4-32.7 |
| V3.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 11.4-32.7 |
| VB.vb_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 11.4-32.7 |
| VB.vb_key_ms.p99 (daemon/voicego) | <= 4 ms | 1.008 | 1.008 | PASS | 11.4-32.7 |
| VB.vb_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 11.4-32.7 |
| VB.vb_stray_speaks (daemon/voicego) | <= 0 utterances | 0 | 0 | PASS | 11.4-32.7 |
| VB.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 11.4-32.7 |
| VF.vf_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 11.4-32.7 |
| VF.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 11.4-32.7 |
| VH.vh_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 11.4-32.7 |
| VH.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 11.4-32.7 |
| VP.vp_echo_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 11.4-32.7 |
| VP.vp_log_lines (daemon/voicego) | >= 1 lines | 34 | 34 | PASS | 11.4-32.7 |
| VP.vp_plaintext_in_logs (daemon/voicego) | <= 0 occurrences | 0 | 0 | PASS | 11.4-32.7 |
| VP.vp_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 11.4-32.7 |
| VP.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 11.4-32.7 |
| VR.vr_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 11.4-32.7 |
| VR.vr_ipc_calls (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 11.4-32.7 |
| VR.vr_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 11.4-32.7 |
| VR.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 11.4-32.7 |
| VT.vt_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 11.4-32.7 |
| VT.vt_extra (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 11.4-32.7 |
| VT.vt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 11.4-32.7 |
| VT.vt_ms.p99 (daemon/voicego) | <= 3 ms | 0.827 | 0.827 | PASS | 11.4-32.7 |
| VT.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 11.4-32.7 |
| VW.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 11.4-32.7 |
| VW.vw_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 11.4-32.7 |
| VW.vw_ms.p99 (daemon/voicego) | <= 35 ms | 32.825 | 32.825 | PASS | 11.4-32.7 |

### m6/vv (attempt 1, load 13.7-22.4, mem PSI some avg10 max 2.750)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| VV.vv_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 13.7-22.4 |
| VV.vv_flood_key_ms.p99 (daemon/voicego) | <= 4 ms | 0.456 | 0.456 | PASS | 13.7-22.4 |
| VV.vv_focus_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 13.7-22.4 |
| VV.vv_key_ms.p99 (daemon/voicego) | <= 3 ms | 0.515 | 0.515 | PASS | 13.7-22.4 |
| VV.vv_password_echo_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.7-22.4 |
| VV.vv_password_echo_extra (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.7-22.4 |
| VV.vv_password_echo_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.7-22.4 |
| VV.vv_password_log_empty (daemon/voicego) | <= 0 files | 0 | 0 | PASS | 13.7-22.4 |
| VV.vv_password_plaintext_in_logs (daemon/voicego) | <= 0 occurrences | 0 | 0 | PASS | 13.7-22.4 |
| VV.vv_password_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.7-22.4 |
| VV.vv_screen_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 13.7-22.4 |
| VV.vv_seq_flood_unspoken (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.7-22.4 |
| VV.vv_seq_key_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.7-22.4 |
| VV.vv_seq_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.7-22.4 |
| VV.vv_server_aborts (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.7-22.4 |
| VV.vv_siocinq_p99_kib (daemon/voicego) | <= 1024 KiB | 0 | 0 | PASS | 13.7-22.4 |
| VV.vv_stop_ms.p99 (daemon/voicego) | <= 2 ms | 0.058 | 0.058 | PASS | 13.7-22.4 |
| VV.vv_type_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.7-22.4 |
| VV.vv_type_extra (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.7-22.4 |
| VV.vv_type_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.7-22.4 |
| VV.vv_worst_cpu_pct (daemon/voicego) | <= 60 % of one core | 1.692 | 1.692 | PASS | 13.7-22.4 |
| VV.vv_yes_flood_unspoken (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.7-22.4 |
| VV.vv_yes_key_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.7-22.4 |
| VV.vv_yes_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 13.7-22.4 |

### m7/firefox (attempt 1, load 11.0-26.2, mem PSI some avg10 max 6.080)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| FC1.fc1_ms.p99 (daemon/voicego) | <= 15 ms | 261.163 | 261.163 | FAIL | 11.0-26.2 |
| FC1.fc_double (daemon/voicego) | <= 0 keys | 2 | 2 | FAIL | 11.0-26.2 |
| FC1.fc_late (daemon/voicego) | <= 0 speeches | 77 | 77 | FAIL | 11.0-26.2 |
| FC1.lost (daemon/voicego) | <= 0 keys | 2 | 2 | FAIL | 11.0-26.2 |
| FC1h.page_dropped (daemon/voicego) | <= 2 keys | 3 | 3 | FAIL | 11.0-26.2 |
| FE1.fe1_ms.p99 (daemon/voicego) | <= 3 ms | 3.854 | 3.854 | FAIL | 11.0-26.2 |
| B0.web_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 11.0-26.2 |
| BAR1.bar_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 11.0-26.2 |
| BL1.bl_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 11.0-26.2 |
| BR1.br1_ms.p99 (daemon/voicego) | <= 5 ms | 0.419 | 0.419 | PASS | 11.0-26.2 |
| BR1.br1c_ms.p99 (daemon/voicego) | <= 5 ms | 0.403 | 0.403 | PASS | 11.0-26.2 |
| BR1.br1w_ms.p99 (daemon/voicego) | <= 5 ms | 0.425 | 0.425 | PASS | 11.0-26.2 |
| BR1.lost_br1_ms (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 11.0-26.2 |
| BR1.lost_br1c_ms (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 11.0-26.2 |
| BR1.lost_br1w_ms (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 11.0-26.2 |
| BR1.not_consumed (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 11.0-26.2 |
| BR2.stale (daemon/voicego) | <= 0 utterances | 0 | 0 | PASS | 11.0-26.2 |
| BSA1.bsa1_ms.p99 (daemon/voicego) | <= 5 ms | 0.291 | 0.291 | PASS | 11.0-26.2 |
| BSA1.bsa_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 11.0-26.2 |
| FC1.fc_extra (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 11.0-26.2 |
| FC1h.fc1h_ms.p99 (daemon/voicego) | <= 3 ms | 0.669 | 0.669 | PASS | 11.0-26.2 |
| FC1h.fc_double (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 11.0-26.2 |
| FC1h.fc_extra (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 11.0-26.2 |
| FC1h.fc_late (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 11.0-26.2 |
| FC1h.fc_swallowed (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 11.0-26.2 |
| FC1h.lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 11.0-26.2 |
| FCE1.fc_double (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 11.0-26.2 |
| FCE1.fc_extra (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 11.0-26.2 |
| FCE1.fc_late (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 11.0-26.2 |
| FCE1.ft_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 11.0-26.2 |
| FCE1.lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 11.0-26.2 |
| FCE2.fc_double (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 11.0-26.2 |
| FCE2.fc_extra (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 11.0-26.2 |
| FCE2.fc_late (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 11.0-26.2 |
| FCE2.fce2_ms.p99 (daemon/voicego) | <= 3 ms | 0.495 | 0.495 | PASS | 11.0-26.2 |
| FCE2.lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 11.0-26.2 |
| FE1.fc_double (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 11.0-26.2 |
| FE1.fc_extra (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 11.0-26.2 |
| FE1.fc_late (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 11.0-26.2 |
| FE1.lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 11.0-26.2 |
| FE2.e2_double (daemon/voicego) | <= 0 actions | 0 | 0 | PASS | 11.0-26.2 |
| FE2.e2_keyname (daemon/voicego) | <= 0 actions | 0 | 0 | PASS | 11.0-26.2 |
| FE2.e2_missing (daemon/voicego) | <= 0 actions | 0 | 0 | PASS | 11.0-26.2 |
| FE2.e2_resync (daemon/voicego) | <= 3 actions | 3 | 3 | PASS | 11.0-26.2 |
| FE2.e2_swallowed (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 11.0-26.2 |
| FE2.e2_wrong (daemon/voicego) | <= 0 actions | 0 | 0 | PASS | 11.0-26.2 |
| FE2.ft_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 11.0-26.2 |
| FM1.fm1_ms.p99 (daemon/voicego) | <= 15 ms | 0.916 | 0.916 | PASS | 11.0-26.2 |
| FM1.fm_grab_ms.p99 (daemon/voicego) | <= 15 ms | 0.980 | 0.980 | PASS | 11.0-26.2 |
| FM1.lost (daemon/voicego) | <= 0 trials | 0 | 0 | PASS | 11.0-26.2 |
| FM1.lost_after_switch (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 11.0-26.2 |
| FM1.not_consumed (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 11.0-26.2 |
| FM2.fm2_ms.p99 (daemon/voicego) | <= 5 ms | 0.437 | 0.437 | PASS | 11.0-26.2 |
| FM2.lost (daemon/voicego) | <= 0 trials | 0 | 0 | PASS | 11.0-26.2 |
| FM2.not_consumed (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 11.0-26.2 |
| FP1.ft_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 11.0-26.2 |
| FR1.fr_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 11.0-26.2 |
| FS2.fc_extra (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 11.0-26.2 |
| FS2.fc_late (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 11.0-26.2 |
| FS2.fs2_ms.p99 (daemon/voicego) | <= 15 ms | 7.558 | 7.558 | PASS | 11.0-26.2 |
| FS2.lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 11.0-26.2 |
| FS2.page_dropped (daemon/voicego) | <= 2 keys | 0 | 0 | PASS | 11.0-26.2 |
| FSA1.fsa1_ms.p99 (daemon/voicego) | <= 15 ms | 1.289 | 1.289 | PASS | 11.0-26.2 |
| FSA1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 11.0-26.2 |
| FSA3.fsa3_stop_ms.p99 (daemon/voicego) | <= 2 ms | 0.334 | 0.334 | PASS | 11.0-26.2 |
| FSA3.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 11.0-26.2 |
| FSA3.sa3_caret_wrong (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 11.0-26.2 |
| FSA3.sa3_not_silenced (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 11.0-26.2 |
| FT1.ft_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 11.0-26.2 |
| W1.lost_w1_ms (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 11.0-26.2 |
| W1.not_consumed (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 11.0-26.2 |
| W1.w1_ms.p99 (daemon/voicego) | <= 5 ms | 0.459 | 0.459 | PASS | 11.0-26.2 |
| W2b.lost (daemon/voicego) | <= 0 trials | 0 | 0 | PASS | 11.0-26.2 |
| W2b.untagged_warm (daemon/voicego) | <= 0 trials | 0 | 0 | PASS | 11.0-26.2 |
| W2b.w2b_gorca_ms.p99 (daemon/voicego) | <= 150 ms | 135.105 | 135.105 | PASS | 11.0-26.2 |
| W3.lost (daemon/voicego) | <= 0 trials | 0 | 0 | PASS | 11.0-26.2 |
| W3.w3_ms.p99 (daemon/voicego) | <= 15 ms | 0.234 | 0.234 | PASS | 11.0-26.2 |

## Host

```
mode=DRY RUN (not acceptance), PARTIAL (m1 m6 m7)
utc=2026-10-16T23:27:05Z
git_rev=c4183c7 dirty_tracked_files=1 branch=worktree-agent-a212e0b0695e3d0d9
kernel=7.1.8+deb13-rt-amd64 cpus=16
loadavg=17.54 16.82 13.82 3/3124 2497517
psi_memory_some_avg10=0.19 psi_memory_full_avg10=0.19
psi_cpu_some_avg10=0.69 psi_io_some_avg10=0.00
mem_available_mb=15618
governor=performance 
rtkit MaxRealtimePriority: i 20 
rtkit RTTimeUSecMax: x 200000 
topology: shipped default (--topology=auto); expected arm: lanes-rt (gated by e2e --expect-topology)
repeats=1 quick=--quick suites=m1 m6 m7
quiet-host:
  governor: performance 
  idle POLL latency=0us disabled=0
  idle C1_ACPI latency=1us disabled=0
  idle C2_ACPI latency=127us disabled=1
  idle C3_ACPI latency=1048us disabled=1
  load: 17.54 16.82 13.82 2/3126 2497557
git_rev_built=c4183c7 binary=e945a7b9567bd5de mtime=2026-10-17 02:18:56.945936891 +0300
loadavg_end=13.36 15.37 15.40 13/2665 2638706 elapsed_s=1103
```
