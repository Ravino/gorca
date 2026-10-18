# Phase 2 acceptance: FAIL

Mode: **ACCEPTANCE, PARTIAL (m3 m6 m9 b1)**. Run dir: `<repo>/bench/results/acceptance/phase2-rerun2-20261018T142938Z`.
Resolved topology (from the reader reports): lanes-rtx3, lanes-rtx39, lanes-rtx45, lanes-rtx87.

## Steps

| step | result | attempt | wall | load1 min-max | mem PSI max | gated rows | FAIL rows | notes |
|---|---|---|---|---|---|---|---|---|
| m3/e2e | PASS | a1 | 2245 s | 2.1-7.8 | 5.020 | 37 | 0 | - |
| m6/fake | PASS | a1 | 2611 s | 1.8-6.6 | 0.140 | 45 | 0 | - |
| m6/vv | PASS | a1 | 383 s | 3.3-7.0 | 0.590 | 24 | 0 | - |
| m9/chromium-m7 | FAIL | a1 | 3538 s | 1.8-7.1 | 3.310 | 77 | 1 | exit 1; 1 gate rows FAIL |
| m9/chromium-m8 | PASS | a1 | 1871 s | 2.2-6.5 | 3.960 | 46 | 0 | - |
| b1/judge-quick | PASS | a1 | 1561 s | 0.3-5.9 | 0.090 | 23 | 0 | - |

## Failures

- m9/chromium-m7: exit 1, 1 gate rows FAIL

## Failing gates

- m9/chromium-m7: FE2.e2_missing median 30 max 30 actions (gate <= 0)

## Gates

### m3/e2e (attempt 1, load 2.1-7.8, mem PSI some avg10 max 5.020)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| C1.c1_ms.p99 (daemon/voicego) | <= 15 ms | 0.605 | 0.615 | PASS | 2.1-7.8 |
| C1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.1-7.8 |
| C1h.c1h_ipc_calls (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 2.1-7.8 |
| C1h.c1h_ms.p99 (daemon/voicego) | <= 3 ms | 0.266 | 0.272 | PASS | 2.1-7.8 |
| C1h.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.1-7.8 |
| C1hs.c1h_ipc_calls (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 2.1-7.8 |
| C1hs.c1hs_ms.p99 (daemon/voicego) | <= 4 ms | 0.288 | 0.313 | PASS | 2.1-7.8 |
| C1hs.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.1-7.8 |
| C2.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.1-7.8 |
| C2.stale_not_cancelled (daemon/voicego) | <= 0 utterances | 0 | 0 | PASS | 2.1-7.8 |
| C3.c3_wrong (daemon/voicego) | <= 0 moves | 0 | 0 | PASS | 2.1-7.8 |
| E1.e1_ms.p99 (daemon/voicego) | <= 3 ms | 0.273 | 0.292 | PASS | 2.1-7.8 |
| E1.e2_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.1-7.8 |
| E1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.1-7.8 |
| E1s.e1s_ms.p99 (daemon/voicego) | <= 4 ms | 0.281 | 0.312 | PASS | 2.1-7.8 |
| E1s.e2_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.1-7.8 |
| E1s.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.1-7.8 |
| E2.e2_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.1-7.8 |
| E2.e2_keyname (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.1-7.8 |
| E2.e2_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.1-7.8 |
| E2.e2_wrong (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.1-7.8 |
| E2f.e2f_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.1-7.8 |
| E2f.e2f_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.1-7.8 |
| PW.pw_echo_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.1-7.8 |
| PW.pw_log_lines (daemon/voicego) | >= 1 lines | 34 | 34 | PASS | 2.1-7.8 |
| PW.pw_plaintext_in_logs (daemon/voicego) | <= 0 occurrences | 0 | 0 | PASS | 2.1-7.8 |
| S2.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.1-7.8 |
| S2.s2_ms.p99 (daemon/voicego) | <= 15 ms | 0.916 | 0.942 | PASS | 2.1-7.8 |
| SA1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.1-7.8 |
| SA1.sa1_ms.p99 (daemon/voicego) | <= 15 ms | 1.092 | 1.221 | PASS | 2.1-7.8 |
| SA2.lost (daemon/voicego) | <= 0 chunks | 0 | 0 | PASS | 2.1-7.8 |
| SA2.sa2_ahead_ratio (daemon/voicego) | >= 0.990 ratio | 1 | 1 | PASS | 2.1-7.8 |
| SA3.sa3_caret_wrong (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.1-7.8 |
| SA3.sa3_not_silenced (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.1-7.8 |
| SA3.sa3_stop_ms.p99 (daemon/voicego) | <= 2 ms | 0.431 | 0.452 | PASS | 2.1-7.8 |
| SA3a.sa3a_caret_moved_back (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.1-7.8 |
| SA3a.sa3a_wrong (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.1-7.8 |

### m6/fake (attempt 1, load 1.8-6.6, mem PSI some avg10 max 0.140)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| V1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 1.8-6.6 |
| V1.v1_ms.p99 (daemon/voicego) | <= 35 ms | 32.556 | 32.594 | PASS | 1.8-6.6 |
| V1.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 1.8-6.6 |
| V2.v2_cat_after_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 1.8-6.6 |
| V2.v2_key_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 1.8-6.6 |
| V2.v2_key_ms.p99 (daemon/voicego) | <= 4 ms | 0.459 | 0.513 | PASS | 1.8-6.6 |
| V2.v2_make_after_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 1.8-6.6 |
| V2.v2_siocinq_measured (daemon/voicego) | >= 1 flag | 1 | 1 | PASS | 1.8-6.6 |
| V2.v2_siocinq_p99_kib (daemon/voicego) | <= 1024 KiB | 0 | 0 | PASS | 1.8-6.6 |
| V2.v2_worst_cpu_pct (daemon/voicego) | <= 60 % of one core | 1.702 | 1.720 | PASS | 1.8-6.6 |
| V2.v2_yes_after_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 1.8-6.6 |
| V2.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 1.8-6.6 |
| V3.v3_no_stop (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 1.8-6.6 |
| V3.v3_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 1.8-6.6 |
| V3.v3_stale (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 1.8-6.6 |
| V3.v3_stop_ms.p99 (daemon/voicego) | <= 2 ms | 0.153 | 0.201 | PASS | 1.8-6.6 |
| V3.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 1.8-6.6 |
| VB.vb_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 1.8-6.6 |
| VB.vb_key_ms.p99 (daemon/voicego) | <= 4 ms | 0.404 | 0.405 | PASS | 1.8-6.6 |
| VB.vb_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 1.8-6.6 |
| VB.vb_stray_speaks (daemon/voicego) | <= 0 utterances | 0 | 0 | PASS | 1.8-6.6 |
| VB.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 1.8-6.6 |
| VF.vf_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 1.8-6.6 |
| VF.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 1.8-6.6 |
| VH.vh_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 1.8-6.6 |
| VH.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 1.8-6.6 |
| VP.vp_echo_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 1.8-6.6 |
| VP.vp_echo_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 1.8-6.6 |
| VP.vp_log_lines (daemon/voicego) | >= 1 lines | 34 | 34 | PASS | 1.8-6.6 |
| VP.vp_plaintext_in_logs (daemon/voicego) | <= 0 occurrences | 0 | 0 | PASS | 1.8-6.6 |
| VP.vp_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 1.8-6.6 |
| VP.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 1.8-6.6 |
| VR.vr_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 1.8-6.6 |
| VR.vr_ipc_calls (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 1.8-6.6 |
| VR.vr_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 1.8-6.6 |
| VR.vr_ms.p99 (daemon/voicego) | <= 3 ms | 0.289 | 0.291 | PASS | 1.8-6.6 |
| VR.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 1.8-6.6 |
| VT.vt_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 1.8-6.6 |
| VT.vt_extra (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 1.8-6.6 |
| VT.vt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 1.8-6.6 |
| VT.vt_ms.p99 (daemon/voicego) | <= 3 ms | 0.475 | 0.476 | PASS | 1.8-6.6 |
| VT.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 1.8-6.6 |
| VW.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 1.8-6.6 |
| VW.vw_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 1.8-6.6 |
| VW.vw_ms.p99 (daemon/voicego) | <= 35 ms | 32.134 | 32.210 | PASS | 1.8-6.6 |

### m6/vv (attempt 1, load 3.3-7.0, mem PSI some avg10 max 0.590)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| VV.vv_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.3-7.0 |
| VV.vv_flood_key_ms.p99 (daemon/voicego) | <= 4 ms | 0.500 | 0.502 | PASS | 3.3-7.0 |
| VV.vv_focus_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.3-7.0 |
| VV.vv_key_ms.p99 (daemon/voicego) | <= 3 ms | 0.437 | 0.440 | PASS | 3.3-7.0 |
| VV.vv_password_echo_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 3.3-7.0 |
| VV.vv_password_echo_extra (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 3.3-7.0 |
| VV.vv_password_echo_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 3.3-7.0 |
| VV.vv_password_log_empty (daemon/voicego) | <= 0 files | 0 | 0 | PASS | 3.3-7.0 |
| VV.vv_password_plaintext_in_logs (daemon/voicego) | <= 0 occurrences | 0 | 0 | PASS | 3.3-7.0 |
| VV.vv_password_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 3.3-7.0 |
| VV.vv_screen_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.3-7.0 |
| VV.vv_seq_flood_unspoken (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 3.3-7.0 |
| VV.vv_seq_key_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 3.3-7.0 |
| VV.vv_seq_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 3.3-7.0 |
| VV.vv_server_aborts (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 3.3-7.0 |
| VV.vv_siocinq_p99_kib (daemon/voicego) | <= 1024 KiB | 0 | 0 | PASS | 3.3-7.0 |
| VV.vv_stop_ms.p99 (daemon/voicego) | <= 2 ms | 0.060 | 0.135 | PASS | 3.3-7.0 |
| VV.vv_type_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 3.3-7.0 |
| VV.vv_type_extra (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 3.3-7.0 |
| VV.vv_type_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 3.3-7.0 |
| VV.vv_worst_cpu_pct (daemon/voicego) | <= 60 % of one core | 1.668 | 1.806 | PASS | 3.3-7.0 |
| VV.vv_yes_flood_unspoken (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 3.3-7.0 |
| VV.vv_yes_key_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 3.3-7.0 |
| VV.vv_yes_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 3.3-7.0 |

### m9/chromium-m7 (attempt 1, load 1.8-7.1, mem PSI some avg10 max 3.310)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| FE2.e2_missing (daemon/voicego) | <= 0 actions | 30 | 30 | FAIL | 1.8-7.1 |
| B0.web_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 1.8-7.1 |
| BAR1.bar_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 1.8-7.1 |
| BL1.bl_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 1.8-7.1 |
| BR1.br1_ms.p99 (daemon/voicego) | <= 5 ms | 0.340 | 0.343 | PASS | 1.8-7.1 |
| BR1.br1c_ms.p99 (daemon/voicego) | <= 5 ms | 0.348 | 0.376 | PASS | 1.8-7.1 |
| BR1.br1w_ms.p99 (daemon/voicego) | <= 5 ms | 0.345 | 0.361 | PASS | 1.8-7.1 |
| BR1.lost_br1_ms (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 1.8-7.1 |
| BR1.lost_br1c_ms (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 1.8-7.1 |
| BR1.lost_br1w_ms (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 1.8-7.1 |
| BR1.not_consumed (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 1.8-7.1 |
| BR2.stale (daemon/voicego) | <= 0 utterances | 0 | 0 | PASS | 1.8-7.1 |
| BSA1.bsa1_ms.p99 (daemon/voicego) | <= 5 ms | 0.268 | 0.333 | PASS | 1.8-7.1 |
| BSA1.bsa_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 1.8-7.1 |
| FC1.fc1_ms.p99 (daemon/voicego) | <= 15 ms | 7.846 | 7.909 | PASS | 1.8-7.1 |
| FC1.fc_double (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 1.8-7.1 |
| FC1.fc_extra (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 1.8-7.1 |
| FC1.fc_late (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 1.8-7.1 |
| FC1.lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 1.8-7.1 |
| FC1h.fc1h_ms.p99 (daemon/voicego) | <= 3 ms | 0.171 | 0.172 | PASS | 1.8-7.1 |
| FC1h.fc_double (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 1.8-7.1 |
| FC1h.fc_extra (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 1.8-7.1 |
| FC1h.fc_late (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 1.8-7.1 |
| FC1h.fc_swallowed (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 1.8-7.1 |
| FC1h.lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 1.8-7.1 |
| FC1h.page_dropped (daemon/voicego) | <= 2 keys | 0 | 0 | PASS | 1.8-7.1 |
| FCE1.fc_double (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 1.8-7.1 |
| FCE1.fc_extra (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 1.8-7.1 |
| FCE1.fc_late (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 1.8-7.1 |
| FCE1.ft_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 1.8-7.1 |
| FCE1.lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 1.8-7.1 |
| FCE2.fc_double (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 1.8-7.1 |
| FCE2.fc_extra (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 1.8-7.1 |
| FCE2.fc_late (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 1.8-7.1 |
| FCE2.fce2_ms.p99 (daemon/voicego) | <= 3 ms | 0.432 | 0.435 | PASS | 1.8-7.1 |
| FCE2.lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 1.8-7.1 |
| FE1.fc_double (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 1.8-7.1 |
| FE1.fc_extra (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 1.8-7.1 |
| FE1.fc_late (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 1.8-7.1 |
| FE1.fe1_ms.p99 (daemon/voicego) | <= 3 ms | 0.412 | 0.426 | PASS | 1.8-7.1 |
| FE1.lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 1.8-7.1 |
| FE2.e2_double (daemon/voicego) | <= 0 actions | 0 | 0 | PASS | 1.8-7.1 |
| FE2.e2_keyname (daemon/voicego) | <= 0 actions | 0 | 0 | PASS | 1.8-7.1 |
| FE2.e2_resync (daemon/voicego) | <= 3 actions | 0 | 0 | PASS | 1.8-7.1 |
| FE2.e2_swallowed (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 1.8-7.1 |
| FE2.e2_wrong (daemon/voicego) | <= 0 actions | 0 | 0 | PASS | 1.8-7.1 |
| FE2.ft_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 1.8-7.1 |
| FM1.fm1_ms.p99 (daemon/voicego) | <= 15 ms | 0.266 | 0.266 | PASS | 1.8-7.1 |
| FM1.fm_grab_ms.p99 (daemon/voicego) | <= 15 ms | 1.022 | 1.075 | PASS | 1.8-7.1 |
| FM1.lost (daemon/voicego) | <= 0 trials | 0 | 0 | PASS | 1.8-7.1 |
| FM1.lost_after_switch (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 1.8-7.1 |
| FM1.not_consumed (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 1.8-7.1 |
| FM2.fm2_ms.p99 (daemon/voicego) | <= 5 ms | 0.484 | 0.515 | PASS | 1.8-7.1 |
| FM2.lost (daemon/voicego) | <= 0 trials | 0 | 0 | PASS | 1.8-7.1 |
| FM2.not_consumed (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 1.8-7.1 |
| FP1.ft_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 1.8-7.1 |
| FR1.fr_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 1.8-7.1 |
| FS2.fc_extra (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 1.8-7.1 |
| FS2.fc_late (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 1.8-7.1 |
| FS2.fs2_ms.p99 (daemon/voicego) | <= 15 ms | 1.191 | 1.208 | PASS | 1.8-7.1 |
| FS2.lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 1.8-7.1 |
| FS2.page_dropped (daemon/voicego) | <= 2 keys | 0 | 0 | PASS | 1.8-7.1 |
| FSA1.fsa1_ms.p99 (daemon/voicego) | <= 15 ms | 1.234 | 1.625 | PASS | 1.8-7.1 |
| FSA1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 1.8-7.1 |
| FSA3.fsa3_stop_ms.p99 (daemon/voicego) | <= 2 ms | 0.414 | 0.419 | PASS | 1.8-7.1 |
| FSA3.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 1.8-7.1 |
| FSA3.sa3_caret_wrong (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 1.8-7.1 |
| FSA3.sa3_not_silenced (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 1.8-7.1 |
| FT1.ft_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 1.8-7.1 |
| W1.lost_w1_ms (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 1.8-7.1 |
| W1.not_consumed (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 1.8-7.1 |
| W1.w1_ms.p99 (daemon/voicego) | <= 5 ms | 0.363 | 0.374 | PASS | 1.8-7.1 |
| W2b.lost (daemon/voicego) | <= 0 trials | 0 | 0 | PASS | 1.8-7.1 |
| W2b.untagged_warm (daemon/voicego) | <= 0 trials | 0 | 0 | PASS | 1.8-7.1 |
| W2b.w2b_gorca_ms.p99 (daemon/voicego) | <= 150 ms | 3.181 | 3.406 | PASS | 1.8-7.1 |
| W3.lost (daemon/voicego) | <= 0 trials | 0 | 0 | PASS | 1.8-7.1 |
| W3.w3_ms.p99 (daemon/voicego) | <= 15 ms | 0.250 | 0.267 | PASS | 1.8-7.1 |

### m9/chromium-m8 (attempt 1, load 2.2-6.5, mem PSI some avg10 max 3.960)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| M8-AF.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-AF.mode_ms.p99 (daemon/voicego) | <= 15 ms | 1.520 | 1.530 | PASS | 2.2-6.5 |
| M8-AN.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-AN.key_ms.p99 (daemon/voicego) | <= 5 ms | 0.411 | 0.412 | PASS | 2.2-6.5 |
| M8-APP.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-BFB.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-BHO.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-BWG.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-BWG.key_ms.p99 (daemon/voicego) | <= 5 ms | 0.281 | 0.284 | PASS | 2.2-6.5 |
| M8-EL.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-FL.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-FLS.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-FM.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-FM.mode_ms.p99 (daemon/voicego) | <= 15 ms | 2.232 | 2.779 | PASS | 2.2-6.5 |
| M8-FORGE.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-FORGE.live_ms.p99 (daemon/voicego) | <= 100 ms | 82.413 | 89.049 | PASS | 2.2-6.5 |
| M8-HD.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-IF.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-LM.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-LM.key_ms.p99 (daemon/voicego) | <= 5 ms | 0.302 | 0.316 | PASS | 2.2-6.5 |
| M8-LV.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-LV.key_ms.p99 (daemon/voicego) | <= 5 ms | 0.405 | 0.489 | PASS | 2.2-6.5 |
| M8-LV.live_ms.p99 (daemon/voicego) | <= 100 ms | 81.051 | 82.460 | PASS | 2.2-6.5 |
| M8-NEWS.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-NEWS.key_ms.p99 (daemon/voicego) | <= 5 ms | 0.300 | 0.345 | PASS | 2.2-6.5 |
| M8-PF1.lost_w1_100k_ms (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-6.5 |
| M8-PF1.w1_100k_ms.p99 (daemon/voicego) | <= 5 ms | 0.375 | 0.397 | PASS | 2.2-6.5 |
| M8-PF2.lost_tb1k_ms (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-6.5 |
| M8-PF2.tb1k_ms.p99 (daemon/voicego) | <= 5 ms | 0.353 | 0.377 | PASS | 2.2-6.5 |
| M8-QN.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-QN.key_ms.p99 (daemon/voicego) | <= 5 ms | 0.396 | 0.396 | PASS | 2.2-6.5 |
| M8-RD.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-RD.key_ms.p99 (daemon/voicego) | <= 5 ms | 0.380 | 0.393 | PASS | 2.2-6.5 |
| M8-RDESC.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-RINV.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-RNAME.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-RNAMES.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-SH.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-SPA.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-SY.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-TB.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-TB.key_ms.p99 (daemon/voicego) | <= 5 ms | 0.241 | 0.389 | PASS | 2.2-6.5 |
| M8-TG.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-TG2.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-TG3.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |
| M8-WIKI.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-6.5 |

### b1/judge-quick (attempt 1, load 0.3-5.9, mem PSI some avg10 max 0.090)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| B1-FF.p99@files (main) | <= 15 ms | 1.299 | 1.299 | PASS | 0.3-5.9 |
| B1-FF.p99@files-big (main) | <= 15 ms | 1.042 | 1.042 | PASS | 0.3-5.9 |
| B1-FF.p99@gtk3 (main) | <= 15 ms | 1.817 | 1.817 | PASS | 0.3-5.9 |
| B1-FF.p99@settings (main) | <= 15 ms | 1.011 | 1.011 | PASS | 0.3-5.9 |
| B1-FF.p99@shell (main) | <= 15 ms | 1.243 | 1.243 | PASS | 0.3-5.9 |
| B1-HIT.revisit (main) | >= 0.950 ratio | 0.956 | 0.956 | PASS | 0.3-5.9 |
| B1-L1c.p99 (main) | <= 15 ms | 3.793 | 3.793 | PASS | 0.3-5.9 |
| B1-L1h.p99 (main) | <= 2 ms | 0.479 | 0.479 | PASS | 0.3-5.9 |
| B1-L3.lost (main) | <= 0 count | 0 | 0 | PASS | 0.3-5.9 |
| B1-L4.p99 (main) | <= 15 ms | 2.516 | 2.516 | PASS | 0.3-5.9 |
| B1-LOST@walk (main) | <= 0 count | 0 | 0 | PASS | 0.3-5.9 |
| B1-M6.audible (main) | <= 0.010 ratio | 0 | 0 | PASS | 0.3-5.9 |
| B1-MSG.idle (main) | <= 0.500 calls/s/app | 0 | 0 | PASS | 0.3-5.9 |
| B1-MSG.l3 (main) | <= 500 calls/s | 0.010 | 0.010 | PASS | 0.3-5.9 |
| B1-RSS.big (main) | <= 128 MiB | 3.113 | 3.113 | PASS | 0.3-5.9 |
| B1-RUNS.invalid (main) | <= 0 runs | 0 | 0 | PASS | 0.3-5.9 |
| B1-RUNS.invalid (main) | <= 0 runs | 0 | 0 | PASS | 0.3-5.9 |
| B1-RUNS.invalid (main) | <= 0 runs | 0 | 0 | PASS | 0.3-5.9 |
| B1-RUNS.invalid (main) | <= 0 runs | 0 | 0 | PASS | 0.3-5.9 |
| B1-RUNS.invalid (main) | <= 0 runs | 0 | 0 | PASS | 0.3-5.9 |
| B1-S1.p99 (main) | <= 5 ms | 2.943 | 2.943 | PASS | 0.3-5.9 |
| B1-STALE@walk (main) | <= 0 count | 0 | 0 | PASS | 0.3-5.9 |
| B1-STEP.max_us (main) | <= 250 us | 15.976 | 17.109 | PASS | 0.3-5.9 |

## Host

```
mode=ACCEPTANCE, PARTIAL (m3 m6 m9 b1)
utc=2026-10-18T14:29:38Z
git_rev=ded46eac dirty_tracked_files=0 branch=main
kernel=7.1.8+deb13-rt-amd64 cpus=16
loadavg=1.54 3.44 5.85 2/2034 2096860
psi_memory_some_avg10=0.00 psi_memory_full_avg10=0.00
psi_cpu_some_avg10=0.01 psi_io_some_avg10=1.11
mem_available_mb=14386
governor=performance 
rtkit MaxRealtimePriority: i 20 
rtkit RTTimeUSecMax: x 200000 
topology: shipped default (--topology=auto); expected arm: lanes-rt (gated by e2e --expect-topology)
repeats=3 quick=no suites=m3 m6 m9 b1
quiet-host:
  governor: performance 
  idle POLL latency=0us disabled=0
  idle C1_ACPI latency=1us disabled=0
  idle C2_ACPI latency=127us disabled=0
  idle C3_ACPI latency=1048us disabled=0
  load: 1.54 3.44 5.85 1/2036 2096901
after quiet-host on:
governor: performance 
idle POLL latency=0us disabled=0
idle C1_ACPI latency=1us disabled=0
idle C2_ACPI latency=127us disabled=1
idle C3_ACPI latency=1048us disabled=1
load: 1.54 3.44 5.85 1/2035 2097051
git_rev_built=ded46eac reader_rev=ded46eac binary=e61a55d2c1a29773 mtime=2026-10-18 17:30:31.182633206 +0300
loadavg_end=0.58 1.03 1.70 1/1984 2259399 elapsed_s=12209
```
