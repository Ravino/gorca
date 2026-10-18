# Phase 2 acceptance: FAIL

Mode: **ACCEPTANCE, PARTIAL (m3 m6)**. Run dir: `<repo>/bench/results/acceptance/phase2-20261018T001009Z`.
Resolved topology (from the reader reports): lanes-rtx24, lanes-rtx3, lanes-rtx30, lanes-rtx39, lanes-rtx45, lanes-rtx6, lanes-rtx87.

## Steps

| step | result | attempt | wall | load1 min-max | mem PSI max | gated rows | FAIL rows | notes |
|---|---|---|---|---|---|---|---|---|
| m3/e2e | PASS | a4 | 2244 s | 2.2-7.8 | 0 | 37 | 0 | 4 attempts, earlier ones kept |
| m6/fake | PASS | a1 | 2610 s | 2.5-9.8 | 7.270 | 45 | 0 | - |
| m6/vv | PASS | a4 | 371 s | 2.6-8.7 | 4.820 | 24 | 0 | 4 attempts, earlier ones kept |
| m9/chromium-m7 | PASS | a1 | 3427 s | 2.2-10.5 | 2.810 | 77 | 0 | - |
| m9/chromium-m8 | FAIL | a2 | 1957 s | 2.2-9.3 | 2.750 | 46 | 2 | exit 1; 2 gate rows FAIL; 2 attempts, earlier ones kept |
| m1/e2e | PASS | a1 | 830 s | 4.0-8.3 | 0.720 | 31 | 0 | - |
| m4/e2e | PASS | a1 | 449 s | 2.9-6.0 | 1.360 | 8 | 0 | - |
| storm/e2e | PASS | a1 | 622 s | 4.1-8.6 | 0.360 | 30 | 0 | - |
| m7/firefox | PASS | a1 | 3658 s | 2.5-8.5 | 2.960 | 77 | 0 | - |
| m8/firefox | PASS | a1 | 2096 s | 3.5-8.3 | 2.750 | 46 | 0 | - |
| x1/hooks | PASS | a1 | 38 s | 4.7-6.2 | 0 | 19 | 0 | - |
| b1/judge-quick | FAIL | a3 | 1492 s | 0.5-4.8 | 1.440 | 23 | 0 | exit 1; 3 attempts, earlier ones kept |

## Failures

- m9/chromium-m8: exit 1, 2 gate rows FAIL, 2 attempts, earlier ones kept
- b1/judge-quick: exit 1, 3 attempts, earlier ones kept

## Failing gates

- m9/chromium-m8: M8-FL.fail median 3 max 3 steps (gate <= 0)
- m9/chromium-m8: M8-FLS.fail median 1 max 1 steps (gate <= 0)

## Gates

### m3/e2e (attempt 4, load 2.2-7.8, mem PSI some avg10 max 0)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| C1.c1_ms.p99 (daemon/voicego) | <= 15 ms | 0.661 | 0.718 | PASS | 2.2-7.8 |
| C1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-7.8 |
| C1h.c1h_ipc_calls (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 2.2-7.8 |
| C1h.c1h_ms.p99 (daemon/voicego) | <= 3 ms | 0.290 | 0.295 | PASS | 2.2-7.8 |
| C1h.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-7.8 |
| C1hs.c1h_ipc_calls (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 2.2-7.8 |
| C1hs.c1hs_ms.p99 (daemon/voicego) | <= 4 ms | 0.283 | 0.312 | PASS | 2.2-7.8 |
| C1hs.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-7.8 |
| C2.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-7.8 |
| C2.stale_not_cancelled (daemon/voicego) | <= 0 utterances | 0 | 0 | PASS | 2.2-7.8 |
| C3.c3_wrong (daemon/voicego) | <= 0 moves | 0 | 0 | PASS | 2.2-7.8 |
| E1.e1_ms.p99 (daemon/voicego) | <= 3 ms | 0.293 | 0.311 | PASS | 2.2-7.8 |
| E1.e2_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-7.8 |
| E1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-7.8 |
| E1s.e1s_ms.p99 (daemon/voicego) | <= 4 ms | 0.301 | 0.311 | PASS | 2.2-7.8 |
| E1s.e2_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-7.8 |
| E1s.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-7.8 |
| E2.e2_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-7.8 |
| E2.e2_keyname (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-7.8 |
| E2.e2_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-7.8 |
| E2.e2_wrong (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-7.8 |
| E2f.e2f_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-7.8 |
| E2f.e2f_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-7.8 |
| PW.pw_echo_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-7.8 |
| PW.pw_log_lines (daemon/voicego) | >= 1 lines | 34 | 34 | PASS | 2.2-7.8 |
| PW.pw_plaintext_in_logs (daemon/voicego) | <= 0 occurrences | 0 | 0 | PASS | 2.2-7.8 |
| S2.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-7.8 |
| S2.s2_ms.p99 (daemon/voicego) | <= 15 ms | 0.891 | 0.955 | PASS | 2.2-7.8 |
| SA1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-7.8 |
| SA1.sa1_ms.p99 (daemon/voicego) | <= 15 ms | 1.164 | 1.278 | PASS | 2.2-7.8 |
| SA2.lost (daemon/voicego) | <= 0 chunks | 0 | 0 | PASS | 2.2-7.8 |
| SA2.sa2_ahead_ratio (daemon/voicego) | >= 0.990 ratio | 1 | 1 | PASS | 2.2-7.8 |
| SA3.sa3_caret_wrong (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-7.8 |
| SA3.sa3_not_silenced (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-7.8 |
| SA3.sa3_stop_ms.p99 (daemon/voicego) | <= 2 ms | 0.400 | 0.502 | PASS | 2.2-7.8 |
| SA3a.sa3a_caret_moved_back (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-7.8 |
| SA3a.sa3a_wrong (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-7.8 |

### m6/fake (attempt 1, load 2.5-9.8, mem PSI some avg10 max 7.270)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| V1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.5-9.8 |
| V1.v1_ms.p99 (daemon/voicego) | <= 35 ms | 32.562 | 32.722 | PASS | 2.5-9.8 |
| V1.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 2.5-9.8 |
| V2.v2_cat_after_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.5-9.8 |
| V2.v2_key_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.5-9.8 |
| V2.v2_key_ms.p99 (daemon/voicego) | <= 4 ms | 0.499 | 0.553 | PASS | 2.5-9.8 |
| V2.v2_make_after_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.5-9.8 |
| V2.v2_siocinq_measured (daemon/voicego) | >= 1 flag | 1 | 1 | PASS | 2.5-9.8 |
| V2.v2_siocinq_p99_kib (daemon/voicego) | <= 1024 KiB | 0 | 0 | PASS | 2.5-9.8 |
| V2.v2_worst_cpu_pct (daemon/voicego) | <= 60 % of one core | 1.738 | 1.844 | PASS | 2.5-9.8 |
| V2.v2_yes_after_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.5-9.8 |
| V2.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 2.5-9.8 |
| V3.v3_no_stop (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.5-9.8 |
| V3.v3_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.5-9.8 |
| V3.v3_stale (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.5-9.8 |
| V3.v3_stop_ms.p99 (daemon/voicego) | <= 2 ms | 0.175 | 0.182 | PASS | 2.5-9.8 |
| V3.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 2.5-9.8 |
| VB.vb_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.5-9.8 |
| VB.vb_key_ms.p99 (daemon/voicego) | <= 4 ms | 0.371 | 0.432 | PASS | 2.5-9.8 |
| VB.vb_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.5-9.8 |
| VB.vb_stray_speaks (daemon/voicego) | <= 0 utterances | 0 | 0 | PASS | 2.5-9.8 |
| VB.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 2.5-9.8 |
| VF.vf_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.5-9.8 |
| VF.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 2.5-9.8 |
| VH.vh_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.5-9.8 |
| VH.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 2.5-9.8 |
| VP.vp_echo_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.5-9.8 |
| VP.vp_echo_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.5-9.8 |
| VP.vp_log_lines (daemon/voicego) | >= 1 lines | 34 | 34 | PASS | 2.5-9.8 |
| VP.vp_plaintext_in_logs (daemon/voicego) | <= 0 occurrences | 0 | 0 | PASS | 2.5-9.8 |
| VP.vp_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.5-9.8 |
| VP.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 2.5-9.8 |
| VR.vr_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.5-9.8 |
| VR.vr_ipc_calls (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 2.5-9.8 |
| VR.vr_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.5-9.8 |
| VR.vr_ms.p99 (daemon/voicego) | <= 3 ms | 0.313 | 0.342 | PASS | 2.5-9.8 |
| VR.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 2.5-9.8 |
| VT.vt_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.5-9.8 |
| VT.vt_extra (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.5-9.8 |
| VT.vt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.5-9.8 |
| VT.vt_ms.p99 (daemon/voicego) | <= 3 ms | 0.527 | 0.536 | PASS | 2.5-9.8 |
| VT.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 2.5-9.8 |
| VW.vte_assert (daemon/voicego) | <= 0 calls | 0 | 0 | PASS | 2.5-9.8 |
| VW.vw_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.5-9.8 |
| VW.vw_ms.p99 (daemon/voicego) | <= 35 ms | 32.129 | 32.312 | PASS | 2.5-9.8 |

### m6/vv (attempt 4, load 2.6-8.7, mem PSI some avg10 max 4.820)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| VV.vv_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.6-8.7 |
| VV.vv_flood_key_ms.p99 (daemon/voicego) | <= 4 ms | 0.521 | 0.559 | PASS | 2.6-8.7 |
| VV.vv_focus_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.6-8.7 |
| VV.vv_key_ms.p99 (daemon/voicego) | <= 3 ms | 0.514 | 0.522 | PASS | 2.6-8.7 |
| VV.vv_password_echo_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.6-8.7 |
| VV.vv_password_echo_extra (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.6-8.7 |
| VV.vv_password_echo_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.6-8.7 |
| VV.vv_password_log_empty (daemon/voicego) | <= 0 files | 0 | 0 | PASS | 2.6-8.7 |
| VV.vv_password_plaintext_in_logs (daemon/voicego) | <= 0 occurrences | 0 | 0 | PASS | 2.6-8.7 |
| VV.vv_password_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.6-8.7 |
| VV.vv_screen_failures (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.6-8.7 |
| VV.vv_seq_flood_unspoken (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.6-8.7 |
| VV.vv_seq_key_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.6-8.7 |
| VV.vv_seq_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.6-8.7 |
| VV.vv_server_aborts (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.6-8.7 |
| VV.vv_siocinq_p99_kib (daemon/voicego) | <= 1024 KiB | 0 | 0 | PASS | 2.6-8.7 |
| VV.vv_stop_ms.p99 (daemon/voicego) | <= 2 ms | 0.140 | 0.176 | PASS | 2.6-8.7 |
| VV.vv_type_double (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.6-8.7 |
| VV.vv_type_extra (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.6-8.7 |
| VV.vv_type_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.6-8.7 |
| VV.vv_worst_cpu_pct (daemon/voicego) | <= 60 % of one core | 1.677 | 1.833 | PASS | 2.6-8.7 |
| VV.vv_yes_flood_unspoken (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.6-8.7 |
| VV.vv_yes_key_lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.6-8.7 |
| VV.vv_yes_prompt_missing (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.6-8.7 |

### m9/chromium-m7 (attempt 1, load 2.2-10.5, mem PSI some avg10 max 2.810)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| B0.web_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 2.2-10.5 |
| BAR1.bar_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 2.2-10.5 |
| BL1.bl_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 2.2-10.5 |
| BR1.br1_ms.p99 (daemon/voicego) | <= 5 ms | 0.352 | 0.354 | PASS | 2.2-10.5 |
| BR1.br1c_ms.p99 (daemon/voicego) | <= 5 ms | 0.346 | 0.354 | PASS | 2.2-10.5 |
| BR1.br1w_ms.p99 (daemon/voicego) | <= 5 ms | 0.345 | 0.387 | PASS | 2.2-10.5 |
| BR1.lost_br1_ms (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-10.5 |
| BR1.lost_br1c_ms (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-10.5 |
| BR1.lost_br1w_ms (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-10.5 |
| BR1.not_consumed (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-10.5 |
| BR2.stale (daemon/voicego) | <= 0 utterances | 0 | 0 | PASS | 2.2-10.5 |
| BSA1.bsa1_ms.p99 (daemon/voicego) | <= 5 ms | 0.322 | 0.390 | PASS | 2.2-10.5 |
| BSA1.bsa_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 2.2-10.5 |
| FC1.fc1_ms.p99 (daemon/voicego) | <= 15 ms | 7.242 | 8.117 | PASS | 2.2-10.5 |
| FC1.fc_double (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-10.5 |
| FC1.fc_extra (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 2.2-10.5 |
| FC1.fc_late (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 2.2-10.5 |
| FC1.lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-10.5 |
| FC1h.fc1h_ms.p99 (daemon/voicego) | <= 3 ms | 0.170 | 0.175 | PASS | 2.2-10.5 |
| FC1h.fc_double (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-10.5 |
| FC1h.fc_extra (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 2.2-10.5 |
| FC1h.fc_late (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 2.2-10.5 |
| FC1h.fc_swallowed (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-10.5 |
| FC1h.lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-10.5 |
| FC1h.page_dropped (daemon/voicego) | <= 2 keys | 0 | 0 | PASS | 2.2-10.5 |
| FCE1.fc_double (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-10.5 |
| FCE1.fc_extra (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 2.2-10.5 |
| FCE1.fc_late (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 2.2-10.5 |
| FCE1.ft_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 2.2-10.5 |
| FCE1.lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-10.5 |
| FCE2.fc_double (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-10.5 |
| FCE2.fc_extra (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 2.2-10.5 |
| FCE2.fc_late (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 2.2-10.5 |
| FCE2.fce2_ms.p99 (daemon/voicego) | <= 3 ms | 0.431 | 0.447 | PASS | 2.2-10.5 |
| FCE2.lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-10.5 |
| FE1.fc_double (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-10.5 |
| FE1.fc_extra (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 2.2-10.5 |
| FE1.fc_late (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 2.2-10.5 |
| FE1.fe1_ms.p99 (daemon/voicego) | <= 3 ms | 0.416 | 0.445 | PASS | 2.2-10.5 |
| FE1.lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-10.5 |
| FE2.e2_double (daemon/voicego) | <= 0 actions | 0 | 0 | PASS | 2.2-10.5 |
| FE2.e2_keyname (daemon/voicego) | <= 0 actions | 0 | 0 | PASS | 2.2-10.5 |
| FE2.e2_missing (daemon/voicego) | <= 0 actions | 0 | 0 | PASS | 2.2-10.5 |
| FE2.e2_resync (daemon/voicego) | <= 3 actions | 0 | 0 | PASS | 2.2-10.5 |
| FE2.e2_swallowed (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-10.5 |
| FE2.e2_wrong (daemon/voicego) | <= 0 actions | 0 | 0 | PASS | 2.2-10.5 |
| FE2.ft_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 2.2-10.5 |
| FM1.fm1_ms.p99 (daemon/voicego) | <= 15 ms | 0.275 | 0.294 | PASS | 2.2-10.5 |
| FM1.fm_grab_ms.p99 (daemon/voicego) | <= 15 ms | 1.056 | 1.163 | PASS | 2.2-10.5 |
| FM1.lost (daemon/voicego) | <= 0 trials | 0 | 0 | PASS | 2.2-10.5 |
| FM1.lost_after_switch (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-10.5 |
| FM1.not_consumed (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-10.5 |
| FM2.fm2_ms.p99 (daemon/voicego) | <= 5 ms | 0.489 | 0.507 | PASS | 2.2-10.5 |
| FM2.lost (daemon/voicego) | <= 0 trials | 0 | 0 | PASS | 2.2-10.5 |
| FM2.not_consumed (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-10.5 |
| FP1.ft_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 2.2-10.5 |
| FR1.fr_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 2.2-10.5 |
| FS2.fc_extra (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 2.2-10.5 |
| FS2.fc_late (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 2.2-10.5 |
| FS2.fs2_ms.p99 (daemon/voicego) | <= 15 ms | 1.209 | 1.211 | PASS | 2.2-10.5 |
| FS2.lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-10.5 |
| FS2.page_dropped (daemon/voicego) | <= 2 keys | 0 | 0 | PASS | 2.2-10.5 |
| FSA1.fsa1_ms.p99 (daemon/voicego) | <= 15 ms | 1.212 | 1.280 | PASS | 2.2-10.5 |
| FSA1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-10.5 |
| FSA3.fsa3_stop_ms.p99 (daemon/voicego) | <= 2 ms | 0.386 | 0.422 | PASS | 2.2-10.5 |
| FSA3.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-10.5 |
| FSA3.sa3_caret_wrong (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-10.5 |
| FSA3.sa3_not_silenced (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.2-10.5 |
| FT1.ft_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 2.2-10.5 |
| W1.lost_w1_ms (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-10.5 |
| W1.not_consumed (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-10.5 |
| W1.w1_ms.p99 (daemon/voicego) | <= 5 ms | 0.353 | 0.355 | PASS | 2.2-10.5 |
| W2b.lost (daemon/voicego) | <= 0 trials | 0 | 0 | PASS | 2.2-10.5 |
| W2b.untagged_warm (daemon/voicego) | <= 0 trials | 0 | 0 | PASS | 2.2-10.5 |
| W2b.w2b_gorca_ms.p99 (daemon/voicego) | <= 150 ms | 1.826 | 2.064 | PASS | 2.2-10.5 |
| W3.lost (daemon/voicego) | <= 0 trials | 0 | 0 | PASS | 2.2-10.5 |
| W3.w3_ms.p99 (daemon/voicego) | <= 15 ms | 0.271 | 0.311 | PASS | 2.2-10.5 |

### m9/chromium-m8 (attempt 2, load 2.2-9.3, mem PSI some avg10 max 2.750)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| M8-FL.fail (daemon/voicego) | <= 0 steps | 3 | 3 | FAIL | 2.2-9.3 |
| M8-FLS.fail (daemon/voicego) | <= 0 steps | 1 | 1 | FAIL | 2.2-9.3 |
| M8-AF.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-AF.mode_ms.p99 (daemon/voicego) | <= 15 ms | 2.229 | 2.546 | PASS | 2.2-9.3 |
| M8-AN.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-AN.key_ms.p99 (daemon/voicego) | <= 5 ms | 0.385 | 0.395 | PASS | 2.2-9.3 |
| M8-APP.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-BFB.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-BHO.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-BWG.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-BWG.key_ms.p99 (daemon/voicego) | <= 5 ms | 0.328 | 0.436 | PASS | 2.2-9.3 |
| M8-EL.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-FM.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-FM.mode_ms.p99 (daemon/voicego) | <= 15 ms | 2.233 | 2.495 | PASS | 2.2-9.3 |
| M8-FORGE.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-FORGE.live_ms.p99 (daemon/voicego) | <= 100 ms | 81.482 | 82.344 | PASS | 2.2-9.3 |
| M8-HD.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-IF.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-LM.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-LM.key_ms.p99 (daemon/voicego) | <= 5 ms | 0.315 | 0.431 | PASS | 2.2-9.3 |
| M8-LV.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-LV.key_ms.p99 (daemon/voicego) | <= 5 ms | 0.389 | 0.487 | PASS | 2.2-9.3 |
| M8-LV.live_ms.p99 (daemon/voicego) | <= 100 ms | 80.921 | 87.545 | PASS | 2.2-9.3 |
| M8-NEWS.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-NEWS.key_ms.p99 (daemon/voicego) | <= 5 ms | 0.338 | 0.415 | PASS | 2.2-9.3 |
| M8-PF1.lost_w1_100k_ms (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-9.3 |
| M8-PF1.w1_100k_ms.p99 (daemon/voicego) | <= 5 ms | 0.397 | 0.406 | PASS | 2.2-9.3 |
| M8-PF2.lost_tb1k_ms (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.2-9.3 |
| M8-PF2.tb1k_ms.p99 (daemon/voicego) | <= 5 ms | 0.407 | 0.431 | PASS | 2.2-9.3 |
| M8-QN.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-QN.key_ms.p99 (daemon/voicego) | <= 5 ms | 0.417 | 0.483 | PASS | 2.2-9.3 |
| M8-RD.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-RD.key_ms.p99 (daemon/voicego) | <= 5 ms | 0.368 | 0.402 | PASS | 2.2-9.3 |
| M8-RDESC.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-RINV.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-RNAME.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-RNAMES.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-SH.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-SPA.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-SY.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-TB.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-TB.key_ms.p99 (daemon/voicego) | <= 5 ms | 0.342 | 0.345 | PASS | 2.2-9.3 |
| M8-TG.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-TG2.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-TG3.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |
| M8-WIKI.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 2.2-9.3 |

### m1/e2e (attempt 1, load 4.0-8.3, mem PSI some avg10 max 0.720)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| K2.k2_ms.p99 (daemon/voicego) | <= 5 ms | 0.198 | 0.203 | PASS | 4.0-8.3 |
| K2.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.0-8.3 |
| K2.not_consumed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.0-8.3 |
| K2s.k2s_ms.p99 (daemon/voicego) | <= 6 ms | 0.229 | 0.232 | PASS | 4.0-8.3 |
| K2s.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.0-8.3 |
| K2s.not_consumed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.0-8.3 |
| K2s.storm_achieved_eps (daemon/voicego) | >= 45000 ev/s | 49999.599 | 49999.781 | PASS | 4.0-8.3 |
| K3.k3_ms.p99 (daemon/voicego) | <= 15 ms | 1.040 | 1.051 | PASS | 4.0-8.3 |
| K3.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.0-8.3 |
| K3.not_consumed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.0-8.3 |
| K4.app_doubled (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 4.0-8.3 |
| K4.app_lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 4.0-8.3 |
| K4.chord_leaked (daemon/voicego) | <= 0 chords | 0 | 0 | PASS | 4.0-8.3 |
| K4.chord_resent_per_10k (daemon/voicego) | <= 5 chords | 0 | 0 | PASS | 4.0-8.3 |
| K4.flap_failed (daemon/voicego) | <= 0 runs | 0 | 0 | PASS | 4.0-8.3 |
| K4.forged_accepted (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 4.0-8.3 |
| K4.restore_failed (daemon/voicego) | <= 0 runs | 0 | 0 | PASS | 4.0-8.3 |
| K4.wait_timeouts (daemon/voicego) | <= 0 waits | 0 | 0 | PASS | 4.0-8.3 |
| K4h.hold_not_stale (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.0-8.3 |
| K4h.stuck_echoed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.0-8.3 |
| K4h.stuck_hint_wrong (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.0-8.3 |
| K4h.stuck_not_healed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.0-8.3 |
| K4h.stuck_not_swallowed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.0-8.3 |
| R1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.0-8.3 |
| R1.not_consumed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.0-8.3 |
| R1.r1_ms.p99 (daemon/voicego) | <= 3 ms | 0.275 | 0.291 | PASS | 4.0-8.3 |
| R1s.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.0-8.3 |
| R1s.not_consumed (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.0-8.3 |
| R1s.r1s_ms.p99 (daemon/voicego) | <= 4 ms | 0.295 | 0.335 | PASS | 4.0-8.3 |
| R1s.storm_achieved_eps (daemon/voicego) | >= 45000 ev/s | 49999.525 | 49999.873 | PASS | 4.0-8.3 |
| RS.rs_failures (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.0-8.3 |

### m4/e2e (attempt 1, load 2.9-6.0, mem PSI some avg10 max 1.360)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| A1.a1_ms.p99 (daemon/voicego) | <= 15 ms | 5.303 | 5.395 | PASS | 2.9-6.0 |
| A1.a1_wrong (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.9-6.0 |
| A1.a1w_ms.p99 (daemon/voicego) | <= 15 ms | 3.653 | 3.722 | PASS | 2.9-6.0 |
| A1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.9-6.0 |
| N1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.9-6.0 |
| N1.n1_focus_ms.p99 (daemon/voicego) | <= 15 ms | 0.445 | 0.566 | PASS | 2.9-6.0 |
| N1.n1_interrupted (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.9-6.0 |
| N1.n1_text_wrong (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.9-6.0 |

### storm/e2e (attempt 1, load 4.1-8.6, mem PSI some avg10 max 0.360)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| K1-50k.k1_ms.p99 (daemon/voicego) | <= 15 ms | 0.290 | 0.294 | PASS | 4.1-8.6 |
| K1-50k.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.1-8.6 |
| K1-50k.storm_achieved_eps (daemon/voicego) | >= 45000 ev/s | 49999.707 | 49999.857 | PASS | 4.1-8.6 |
| K1.k1_ms.p99 (daemon/voicego) | <= 15 ms | 0.260 | 0.282 | PASS | 4.1-8.6 |
| K1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.1-8.6 |
| L1.l1_ms.p99 (daemon/voicego) | <= 15 ms | 5.291 | 5.306 | PASS | 4.1-8.6 |
| L1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.1-8.6 |
| L1s.l1_ms.p99 (daemon/voicego) | <= 15 ms | 5.334 | 5.356 | PASS | 4.1-8.6 |
| L1s.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.1-8.6 |
| L2-50k.l2_ms.p99 (daemon/voicego) | <= 5 ms | 0.325 | 0.327 | PASS | 4.1-8.6 |
| L2-50k.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.1-8.6 |
| L2-50k.stale_not_cancelled (daemon/voicego) | <= 0 trials | 0 | 0 | PASS | 4.1-8.6 |
| L2-50k.storm_achieved_eps (daemon/voicego) | >= 45000 ev/s | 49999.789 | 49999.944 | PASS | 4.1-8.6 |
| L2.l2_ms.p99 (daemon/voicego) | <= 5 ms | 0.303 | 0.310 | PASS | 4.1-8.6 |
| L2.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.1-8.6 |
| L2.stale_not_cancelled (daemon/voicego) | <= 0 trials | 0 | 0 | PASS | 4.1-8.6 |
| L3-10k.l1_ms.p99 (daemon/voicego) | <= 15 ms | 5.279 | 5.287 | PASS | 4.1-8.6 |
| L3-10k.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.1-8.6 |
| L3-10k.storm_achieved_eps (daemon/voicego) | >= 9000 ev/s | 9999.944 | 9999.980 | PASS | 4.1-8.6 |
| L3-10k.superseded_max_run (daemon/voicego) | <= 10 events | 0 | 0 | PASS | 4.1-8.6 |
| L3-50k.l1_ms.p99 (daemon/voicego) | <= 15 ms | 5.327 | 5.350 | PASS | 4.1-8.6 |
| L3-50k.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.1-8.6 |
| L3-50k.storm_achieved_eps (daemon/voicego) | >= 45000 ev/s | 49999.606 | 49999.839 | PASS | 4.1-8.6 |
| L3-50k.superseded_max_run (daemon/voicego) | <= 10 events | 0 | 0 | PASS | 4.1-8.6 |
| L4.l1_ms.p99 (daemon/voicego) | <= 15 ms | 5.299 | 5.305 | PASS | 4.1-8.6 |
| L4.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.1-8.6 |
| L4.peer_calls_hung (daemon/voicego) | >= 1 calls | 2 | 38 | PASS | 4.1-8.6 |
| L4.superseded_max_run (daemon/voicego) | <= 10 events | 0 | 0 | PASS | 4.1-8.6 |
| L4c.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 4.1-8.6 |
| L4c.superseded_max_run (daemon/voicego) | <= 10 events | 0 | 0 | PASS | 4.1-8.6 |

### m7/firefox (attempt 1, load 2.5-8.5, mem PSI some avg10 max 2.960)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| B0.web_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 2.5-8.5 |
| BAR1.bar_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 2.5-8.5 |
| BL1.bl_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 2.5-8.5 |
| BR1.br1_ms.p99 (daemon/voicego) | <= 5 ms | 0.363 | 0.422 | PASS | 2.5-8.5 |
| BR1.br1c_ms.p99 (daemon/voicego) | <= 5 ms | 0.376 | 0.389 | PASS | 2.5-8.5 |
| BR1.br1w_ms.p99 (daemon/voicego) | <= 5 ms | 0.336 | 0.376 | PASS | 2.5-8.5 |
| BR1.lost_br1_ms (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.5-8.5 |
| BR1.lost_br1c_ms (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.5-8.5 |
| BR1.lost_br1w_ms (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.5-8.5 |
| BR1.not_consumed (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.5-8.5 |
| BR2.stale (daemon/voicego) | <= 0 utterances | 0 | 0 | PASS | 2.5-8.5 |
| BSA1.bsa1_ms.p99 (daemon/voicego) | <= 5 ms | 0.288 | 0.291 | PASS | 2.5-8.5 |
| BSA1.bsa_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 2.5-8.5 |
| FC1.fc1_ms.p99 (daemon/voicego) | <= 15 ms | 0.606 | 0.657 | PASS | 2.5-8.5 |
| FC1.fc_double (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.5-8.5 |
| FC1.fc_extra (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 2.5-8.5 |
| FC1.fc_late (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 2.5-8.5 |
| FC1.lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.5-8.5 |
| FC1h.fc1h_ms.p99 (daemon/voicego) | <= 3 ms | 0.170 | 0.190 | PASS | 2.5-8.5 |
| FC1h.fc_double (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.5-8.5 |
| FC1h.fc_extra (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 2.5-8.5 |
| FC1h.fc_late (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 2.5-8.5 |
| FC1h.fc_swallowed (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.5-8.5 |
| FC1h.lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.5-8.5 |
| FC1h.page_dropped (daemon/voicego) | <= 2 keys | 0 | 0 | PASS | 2.5-8.5 |
| FCE1.fc_double (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.5-8.5 |
| FCE1.fc_extra (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 2.5-8.5 |
| FCE1.fc_late (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 2.5-8.5 |
| FCE1.ft_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 2.5-8.5 |
| FCE1.lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.5-8.5 |
| FCE2.fc_double (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.5-8.5 |
| FCE2.fc_extra (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 2.5-8.5 |
| FCE2.fc_late (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 2.5-8.5 |
| FCE2.fce2_ms.p99 (daemon/voicego) | <= 3 ms | 0.397 | 0.398 | PASS | 2.5-8.5 |
| FCE2.lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.5-8.5 |
| FE1.fc_double (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.5-8.5 |
| FE1.fc_extra (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 2.5-8.5 |
| FE1.fc_late (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 2.5-8.5 |
| FE1.fe1_ms.p99 (daemon/voicego) | <= 3 ms | 0.383 | 0.425 | PASS | 2.5-8.5 |
| FE1.lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.5-8.5 |
| FE2.e2_double (daemon/voicego) | <= 0 actions | 0 | 0 | PASS | 2.5-8.5 |
| FE2.e2_keyname (daemon/voicego) | <= 0 actions | 0 | 0 | PASS | 2.5-8.5 |
| FE2.e2_missing (daemon/voicego) | <= 0 actions | 0 | 0 | PASS | 2.5-8.5 |
| FE2.e2_resync (daemon/voicego) | <= 3 actions | 0 | 2 | PASS | 2.5-8.5 |
| FE2.e2_swallowed (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.5-8.5 |
| FE2.e2_wrong (daemon/voicego) | <= 0 actions | 0 | 0 | PASS | 2.5-8.5 |
| FE2.ft_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 2.5-8.5 |
| FM1.fm1_ms.p99 (daemon/voicego) | <= 15 ms | 0.260 | 0.276 | PASS | 2.5-8.5 |
| FM1.fm_grab_ms.p99 (daemon/voicego) | <= 15 ms | 1.046 | 1.084 | PASS | 2.5-8.5 |
| FM1.lost (daemon/voicego) | <= 0 trials | 0 | 0 | PASS | 2.5-8.5 |
| FM1.lost_after_switch (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.5-8.5 |
| FM1.not_consumed (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.5-8.5 |
| FM2.fm2_ms.p99 (daemon/voicego) | <= 5 ms | 0.467 | 0.470 | PASS | 2.5-8.5 |
| FM2.lost (daemon/voicego) | <= 0 trials | 0 | 0 | PASS | 2.5-8.5 |
| FM2.not_consumed (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.5-8.5 |
| FP1.ft_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 2.5-8.5 |
| FR1.fr_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 2.5-8.5 |
| FS2.fc_extra (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 2.5-8.5 |
| FS2.fc_late (daemon/voicego) | <= 0 speeches | 0 | 0 | PASS | 2.5-8.5 |
| FS2.fs2_ms.p99 (daemon/voicego) | <= 15 ms | 1.062 | 1.080 | PASS | 2.5-8.5 |
| FS2.lost (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.5-8.5 |
| FS2.page_dropped (daemon/voicego) | <= 2 keys | 0 | 0 | PASS | 2.5-8.5 |
| FSA1.fsa1_ms.p99 (daemon/voicego) | <= 15 ms | 1.585 | 3.259 | PASS | 2.5-8.5 |
| FSA1.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.5-8.5 |
| FSA3.fsa3_stop_ms.p99 (daemon/voicego) | <= 2 ms | 0.424 | 0.447 | PASS | 2.5-8.5 |
| FSA3.lost (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.5-8.5 |
| FSA3.sa3_caret_wrong (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.5-8.5 |
| FSA3.sa3_not_silenced (daemon/voicego) | <= 0 events | 0 | 0 | PASS | 2.5-8.5 |
| FT1.ft_failures (daemon/voicego) | <= 0 checks | 0 | 0 | PASS | 2.5-8.5 |
| W1.lost_w1_ms (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.5-8.5 |
| W1.not_consumed (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 2.5-8.5 |
| W1.w1_ms.p99 (daemon/voicego) | <= 5 ms | 0.378 | 0.400 | PASS | 2.5-8.5 |
| W2b.lost (daemon/voicego) | <= 0 trials | 0 | 0 | PASS | 2.5-8.5 |
| W2b.untagged_warm (daemon/voicego) | <= 0 trials | 0 | 0 | PASS | 2.5-8.5 |
| W2b.w2b_gorca_ms.p99 (daemon/voicego) | <= 150 ms | 69.510 | 70.593 | PASS | 2.5-8.5 |
| W3.lost (daemon/voicego) | <= 0 trials | 0 | 0 | PASS | 2.5-8.5 |
| W3.w3_ms.p99 (daemon/voicego) | <= 15 ms | 0.244 | 0.272 | PASS | 2.5-8.5 |

### m8/firefox (attempt 1, load 3.5-8.3, mem PSI some avg10 max 2.750)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| M8-AF.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-AF.mode_ms.p99 (daemon/voicego) | <= 15 ms | 1.237 | 1.243 | PASS | 3.5-8.3 |
| M8-AN.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-AN.key_ms.p99 (daemon/voicego) | <= 5 ms | 0.404 | 0.419 | PASS | 3.5-8.3 |
| M8-APP.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-BFB.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-BHO.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-BWG.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-BWG.key_ms.p99 (daemon/voicego) | <= 5 ms | 0.302 | 0.335 | PASS | 3.5-8.3 |
| M8-EL.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-FL.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-FLS.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-FM.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-FM.mode_ms.p99 (daemon/voicego) | <= 15 ms | 2.025 | 2.323 | PASS | 3.5-8.3 |
| M8-FORGE.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-FORGE.live_ms.p99 (daemon/voicego) | <= 100 ms | 80.044 | 80.555 | PASS | 3.5-8.3 |
| M8-HD.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-IF.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-LM.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-LM.key_ms.p99 (daemon/voicego) | <= 5 ms | 0.323 | 0.330 | PASS | 3.5-8.3 |
| M8-LV.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-LV.key_ms.p99 (daemon/voicego) | <= 5 ms | 0.397 | 0.438 | PASS | 3.5-8.3 |
| M8-LV.live_ms.p99 (daemon/voicego) | <= 100 ms | 87.142 | 91.286 | PASS | 3.5-8.3 |
| M8-NEWS.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-NEWS.key_ms.p99 (daemon/voicego) | <= 5 ms | 0.328 | 0.355 | PASS | 3.5-8.3 |
| M8-PF1.lost_w1_100k_ms (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 3.5-8.3 |
| M8-PF1.w1_100k_ms.p99 (daemon/voicego) | <= 5 ms | 0.394 | 0.405 | PASS | 3.5-8.3 |
| M8-PF2.lost_tb1k_ms (daemon/voicego) | <= 0 keys | 0 | 0 | PASS | 3.5-8.3 |
| M8-PF2.tb1k_ms.p99 (daemon/voicego) | <= 5 ms | 0.415 | 0.428 | PASS | 3.5-8.3 |
| M8-QN.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-QN.key_ms.p99 (daemon/voicego) | <= 5 ms | 0.386 | 0.402 | PASS | 3.5-8.3 |
| M8-RD.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-RD.key_ms.p99 (daemon/voicego) | <= 5 ms | 0.365 | 0.445 | PASS | 3.5-8.3 |
| M8-RDESC.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-RINV.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-RNAME.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-RNAMES.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-SH.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-SPA.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-SY.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-TB.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-TB.key_ms.p99 (daemon/voicego) | <= 5 ms | 0.222 | 0.298 | PASS | 3.5-8.3 |
| M8-TG.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-TG2.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-TG3.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |
| M8-WIKI.fail (daemon/voicego) | <= 0 steps | 0 | 0 | PASS | 3.5-8.3 |

### x1/hooks (attempt 1, load 4.7-6.2, mem PSI some avg10 max 0)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| X1.native-crash_reset_next_call.p99 | <= 50000 us | 26.309 | 29.563 | PASS | 4.7-6.2 |
| X1.native-on_event_focus.p99 | <= 100 us | 0.075 | 0.075 | PASS | 4.7-6.2 |
| X1.native-on_event_pass.p99 | <= 100 us | 0.087 | 0.087 | PASS | 4.7-6.2 |
| X1.native-on_event_rewrite.p99 | <= 100 us | 0.128 | 0.128 | PASS | 4.7-6.2 |
| X1.native-present_none.p99 | <= 100 us | 0.085 | 0.088 | PASS | 4.7-6.2 |
| X1.native-present_some.p99 | <= 100 us | 0.145 | 0.148 | PASS | 4.7-6.2 |
| X1.native-storm_on_event_1t.p99 | <= 100 us | 0.092 | 0.092 | PASS | 4.7-6.2 |
| X1.native-storm_on_event_4t.p99 | <= 100 us | 0.325 | 0.359 | PASS | 4.7-6.2 |
| X1.native-storm_on_event_8t.p99 | <= 100 us | 0.489 | 0.564 | PASS | 4.7-6.2 |
| X1.wasm-crash_reset_next_call.p99 | <= 50000 us | 103.601 | 106.546 | PASS | 4.7-6.2 |
| X1.wasm-on_event_focus.p99 | <= 100 us | 0.233 | 0.236 | PASS | 4.7-6.2 |
| X1.wasm-on_event_pass.p99 | <= 100 us | 0.362 | 0.366 | PASS | 4.7-6.2 |
| X1.wasm-on_event_rewrite.p99 | <= 100 us | 0.459 | 0.459 | PASS | 4.7-6.2 |
| X1.wasm-present_none.p99 | <= 100 us | 0.309 | 0.313 | PASS | 4.7-6.2 |
| X1.wasm-present_some.p99 | <= 100 us | 0.475 | 0.480 | PASS | 4.7-6.2 |
| X1.wasm-spin_interrupt_reset_next_call.p99 | <= 50000 us | 441.837 | 451.902 | PASS | 4.7-6.2 |
| X1.wasm-storm_on_event_1t.p99 | <= 100 us | 0.345 | 0.362 | PASS | 4.7-6.2 |
| X1.wasm-storm_on_event_4t.p99 | <= 100 us | 0.490 | 0.508 | PASS | 4.7-6.2 |
| X1.wasm-storm_on_event_8t.p99 | <= 100 us | 0.741 | 0.741 | PASS | 4.7-6.2 |

### b1/judge-quick (attempt 3, load 0.5-4.8, mem PSI some avg10 max 1.440)

| gate | threshold | median | max | verdict | load |
|---|---|---|---|---|---|
| B1-FF.p99@files (main) | <= 15 ms | 1.362 | 1.362 | PASS | 0.5-4.8 |
| B1-FF.p99@files-big (main) | <= 15 ms | 1.163 | 1.163 | PASS | 0.5-4.8 |
| B1-FF.p99@gtk3 (main) | <= 15 ms | 1.768 | 1.768 | PASS | 0.5-4.8 |
| B1-FF.p99@settings (main) | <= 15 ms | 1.111 | 1.111 | PASS | 0.5-4.8 |
| B1-FF.p99@shell (main) | <= 15 ms | 1.220 | 1.220 | PASS | 0.5-4.8 |
| B1-HIT.revisit (main) | >= 0.950 ratio | 0.956 | 0.956 | PASS | 0.5-4.8 |
| B1-L1c.p99 (main) | <= 15 ms | 3.987 | 3.987 | PASS | 0.5-4.8 |
| B1-L1h.p99 (main) | <= 2 ms | 0.519 | 0.519 | PASS | 0.5-4.8 |
| B1-L3.lost (main) | <= 0 count | 0 | 0 | PASS | 0.5-4.8 |
| B1-L4.p99 (main) | <= 15 ms | 2.589 | 2.589 | PASS | 0.5-4.8 |
| B1-LOST@walk (main) | <= 0 count | 0 | 0 | PASS | 0.5-4.8 |
| B1-M6.audible (main) | <= 0.010 ratio | 0 | 0 | PASS | 0.5-4.8 |
| B1-MSG.idle (main) | <= 0.500 calls/s/app | 0 | 0 | PASS | 0.5-4.8 |
| B1-MSG.l3 (main) | <= 500 calls/s | 0.010 | 0.010 | PASS | 0.5-4.8 |
| B1-RSS.big (main) | <= 128 MiB | 3.125 | 3.125 | PASS | 0.5-4.8 |
| B1-RUNS.invalid (main) | <= 0 runs | 0 | 0 | PASS | 0.5-4.8 |
| B1-RUNS.invalid (main) | <= 0 runs | 0 | 0 | PASS | 0.5-4.8 |
| B1-RUNS.invalid (main) | <= 0 runs | 0 | 0 | PASS | 0.5-4.8 |
| B1-RUNS.invalid (main) | <= 0 runs | 0 | 0 | PASS | 0.5-4.8 |
| B1-RUNS.invalid (main) | <= 0 runs | 0 | 0 | PASS | 0.5-4.8 |
| B1-S1.p99 (main) | <= 5 ms | 3.311 | 3.311 | PASS | 0.5-4.8 |
| B1-STALE@walk (main) | <= 0 count | 0 | 0 | PASS | 0.5-4.8 |
| B1-STEP.max_us (main) | <= 250 us | 15.916 | 19.744 | PASS | 0.5-4.8 |

## Host

```
mode=ACCEPTANCE
utc=2026-10-18T00:10:09Z
git_rev=1308ae05 dirty_tracked_files=0 branch=main
kernel=7.1.8+deb13-rt-amd64 cpus=16
loadavg=3.00 4.72 7.25 3/1940 818044
psi_memory_some_avg10=0.30 psi_memory_full_avg10=0.30
psi_cpu_some_avg10=0.00 psi_io_some_avg10=1.88
mem_available_mb=14800
governor=performance 
rtkit MaxRealtimePriority: i 20 
rtkit RTTimeUSecMax: x 200000 
topology: shipped default (--topology=auto); expected arm: lanes-rt (gated by e2e --expect-topology)
repeats=3 quick=no suites=m1 m3 m4 m6 m7 m8 m9 storm x1 b1
quiet-host:
  governor: performance 
  idle POLL latency=0us disabled=0
  idle C1_ACPI latency=1us disabled=0
  idle C2_ACPI latency=127us disabled=1
  idle C3_ACPI latency=1048us disabled=1
  load: 3.00 4.72 7.25 3/1935 818084
after quiet-host on:
governor: performance 
idle POLL latency=0us disabled=0
idle C1_ACPI latency=1us disabled=0
idle C2_ACPI latency=127us disabled=1
idle C3_ACPI latency=1048us disabled=1
load: 3.00 4.72 7.25 1/1934 818234
git_rev_built=1308ae05 binary=321b3ed0d392dddf mtime=2026-10-18 03:11:02.236923184 +0300
loadavg_end=2.13 2.14 2.03 2/2175 1148219 elapsed_s=23643
---- resumed ----
mode=ACCEPTANCE
utc=2026-10-18T06:47:23Z
git_rev=b63baa06 dirty_tracked_files=0 branch=main
kernel=7.1.8+deb13-rt-amd64 cpus=16
loadavg=2.40 2.17 2.04 1/2178 1148982
psi_memory_some_avg10=0.00 psi_memory_full_avg10=0.00
psi_cpu_some_avg10=0.20 psi_io_some_avg10=0.00
mem_available_mb=14728
governor=performance 
rtkit MaxRealtimePriority: i 20 
rtkit RTTimeUSecMax: x 200000 
topology: shipped default (--topology=auto); expected arm: lanes-rt (gated by e2e --expect-topology)
repeats=3 quick=no suites=m1 m3 m4 m6 m7 m8 m9 storm x1 b1
quiet-host:
  governor: performance 
  idle POLL latency=0us disabled=0
  idle C1_ACPI latency=1us disabled=0
  idle C2_ACPI latency=127us disabled=0
  idle C3_ACPI latency=1048us disabled=0
  load: 2.40 2.17 2.04 1/2180 1149022
after quiet-host on:
governor: performance 
idle POLL latency=0us disabled=0
idle C1_ACPI latency=1us disabled=0
idle C2_ACPI latency=127us disabled=1
idle C3_ACPI latency=1048us disabled=1
load: 2.40 2.17 2.04 1/2178 1149172
git_rev_built=b63baa06 binary=321b3ed0d392dddf mtime=2026-10-18 05:01:57.471879776 +0300
loadavg_end=2.46 2.14 2.45 2/2161 1201723 elapsed_s=3456
---- resumed ----
mode=ACCEPTANCE, PARTIAL (m1 m3 m4 m6 m7 m8 storm x1 b1)
utc=2026-10-18T07:54:09Z
git_rev=28c9aa47 dirty_tracked_files=0 branch=main
kernel=7.1.8+deb13-rt-amd64 cpus=16
loadavg=4.92 4.75 3.66 4/2183 1207457
psi_memory_some_avg10=0.00 psi_memory_full_avg10=0.00
psi_cpu_some_avg10=0.20 psi_io_some_avg10=0.19
mem_available_mb=14900
governor=performance 
rtkit MaxRealtimePriority: i 20 
rtkit RTTimeUSecMax: x 200000 
topology: shipped default (--topology=auto); expected arm: lanes-rt (gated by e2e --expect-topology)
repeats=3 quick=no suites=m1 m3 m4 m6 m7 m8 storm x1 b1
quiet-host:
  governor: performance 
  idle POLL latency=0us disabled=0
  idle C1_ACPI latency=1us disabled=0
  idle C2_ACPI latency=127us disabled=0
  idle C3_ACPI latency=1048us disabled=0
  load: 4.92 4.75 3.66 2/2186 1207498
---- resumed ----
mode=ACCEPTANCE, PARTIAL (m1 m3 m4 m6 m7 m8 storm x1 b1)
utc=2026-10-18T07:55:17Z
git_rev=28c9aa47 dirty_tracked_files=2 branch=main
kernel=7.1.8+deb13-rt-amd64 cpus=16
loadavg=2.66 4.16 3.54 2/2171 1207926
psi_memory_some_avg10=0.00 psi_memory_full_avg10=0.00
psi_cpu_some_avg10=0.08 psi_io_some_avg10=0.00
mem_available_mb=15069
governor=performance 
rtkit MaxRealtimePriority: i 20 
rtkit RTTimeUSecMax: x 200000 
topology: shipped default (--topology=auto); expected arm: lanes-rt (gated by e2e --expect-topology)
repeats=3 quick=no suites=m1 m3 m4 m6 m7 m8 storm x1 b1
quiet-host:
  governor: performance 
  idle POLL latency=0us disabled=0
  idle C1_ACPI latency=1us disabled=0
  idle C2_ACPI latency=127us disabled=0
  idle C3_ACPI latency=1048us disabled=0
  load: 2.66 4.16 3.54 2/2173 1207966
after quiet-host on:
governor: performance 
idle POLL latency=0us disabled=0
idle C1_ACPI latency=1us disabled=0
idle C2_ACPI latency=127us disabled=1
idle C3_ACPI latency=1048us disabled=1
load: 2.66 4.16 3.54 1/2172 1208116
git_rev_built=28c9aa47 binary=712744f52e6b10ad mtime=2026-10-18 10:47:59.393508922 +0300
loadavg_end=1.55 1.49 2.12 1/2166 1220590 elapsed_s=1498
---- resumed ----
mode=ACCEPTANCE, PARTIAL (m3 m6)
utc=2026-10-18T08:22:26Z
git_rev=28c9aa47 dirty_tracked_files=5 branch=main
kernel=7.1.8+deb13-rt-amd64 cpus=16
loadavg=2.59 2.13 2.29 2/2181 1221786
psi_memory_some_avg10=0.00 psi_memory_full_avg10=0.00
psi_cpu_some_avg10=0.55 psi_io_some_avg10=0.00
mem_available_mb=14869
governor=performance 
rtkit MaxRealtimePriority: i 20 
rtkit RTTimeUSecMax: x 200000 
topology: shipped default (--topology=auto); expected arm: lanes-rt (gated by e2e --expect-topology)
repeats=3 quick=no suites=m3 m6
quiet-host:
  governor: performance 
  idle POLL latency=0us disabled=0
  idle C1_ACPI latency=1us disabled=0
  idle C2_ACPI latency=127us disabled=0
  idle C3_ACPI latency=1048us disabled=0
  load: 2.59 2.13 2.29 1/2183 1221826
after quiet-host on:
governor: performance 
idle POLL latency=0us disabled=0
idle C1_ACPI latency=1us disabled=0
idle C2_ACPI latency=127us disabled=1
idle C3_ACPI latency=1048us disabled=1
load: 2.59 2.13 2.29 1/2182 1221980
git_rev_built=28c9aa47 binary=3ca5c98205834944 mtime=2026-10-18 11:21:36.536463335 +0300
loadavg_end=5.05 4.83 4.36 1/2165 1244103 elapsed_s=2615
```
