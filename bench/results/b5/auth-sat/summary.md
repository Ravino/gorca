# B-5 judge summary: auth-sat

```
label=auth-sat
utc=2026-10-16T09:02:36Z
git_rev=1993580 dirty=2
kernel=7.1.8+deb13-rt-amd64
cpus=16
governor=performance 
loadavg_start=15.31 14.78 13.96 23/3526 3309616
rounds=5 arms=single,lanes,lanes-rt conds=sat only=L1,L2,K1,L3-50k,L2-50k,K1-50k,K2,K2s,K3,R1,R1s,A1 quick=no
rtkit MaxRealtimePriority: i 20 
rtkit MinNiceLevel: i -15 
rtkit RTTimeUSecMax: x 200000 
RLIMIT_RTPRIO=0 RLIMIT_RTTIME=unlimited
quiet-host:
  governor: performance 
  idle POLL latency=0us disabled=0
  idle C1_ACPI latency=1us disabled=0
  idle C2_ACPI latency=127us disabled=1
  idle C3_ACPI latency=1048us disabled=1
  load: 15.31 14.78 13.96 17/3530 3309687
loadavg_end=74.95 68.65 65.86 55/3276 3818751
```

## Condition `sat`

Value = median over rounds [min..max]; `W` = rounds in which the arm was lower than `single` in the same round / rounds.

| series | pct | single | lanes | lanes-rt |
|---|---|---|---|---|
| L1 l1_ms | p50 | 5.803 [5.501..5.936] | 5.849 [5.693..6.421] W2/5 | 5.322 [5.317..5.443] W5/5 |
| L1 l1_ms | p95 | 10.312 [9.082..12.158] | 10.264 [8.889..13.899] W3/5 | 7.864 [6.889..9.750] W4/5 |
| L1 l1_ms | p99 | 13.081 [11.915..22.786] | 14.196 [12.021..16.713] W3/5 | 11.330 [8.495..12.992] W4/5 |
| L2 l2_ms | p50 | 0.843 [0.629..0.919] | 0.544 [0.499..0.574] W5/5 | 0.382 [0.376..0.417] W5/5 |
| L2 l2_ms | p95 | 6.479 [4.876..6.675] | 5.312 [4.063..6.518] W4/5 | 3.891 [3.171..4.519] W5/5 |
| L2 l2_ms | p99 | 10.491 [8.162..13.842] | 8.360 [7.071..10.743] W3/5 | 7.118 [5.129..8.257] W5/5 |
| K1 k1_ms | p50 | 0.442 [0.388..0.535] | 0.492 [0.438..2.041] W1/5 | 0.314 [0.292..0.416] W5/5 |
| K1 k1_ms | p95 | 5.049 [3.569..359.492] | 7.084 [4.043..316.996] W3/5 | 2.216 [2.000..3.050] W5/5 |
| K1 k1_ms | p99 | 353.004 [6.366..724.231] | 9.935 [6.242..349.316] W3/5 | 3.872 [3.611..14.224] W5/5 |
| L3-50k l1_ms | p50 | 10.192 [7.466..12.235] | 7.224 [6.671..11.947] W4/5 | 8.160 [5.854..9.569] W4/5 |
| L3-50k l1_ms | p95 | 33.432 [19.825..308.688] | 32.756 [17.906..256.439] W3/5 | 14.160 [10.395..18.507] W5/5 |
| L3-50k l1_ms | p99 | 197.609 [25.092..676.814] | 89.396 [35.453..527.201] W3/5 | 17.311 [14.041..25.061] W5/5 |
| L2-50k l2_ms | p50 | 5.437 [4.191..8.227] | 2.422 [1.630..7.142] W3/5 | 2.432 [1.301..2.852] W5/5 |
| L2-50k l2_ms | p95 | 99.915 [51.344..356.518] | 29.900 [10.491..373.425] W3/5 | 8.225 [5.241..8.915] W5/5 |
| L2-50k l2_ms | p99 | 217.926 [191.854..743.255] | 154.830 [58.821..685.835] W3/5 | 13.186 [11.446..13.742] W5/5 |
| K1-50k k1_ms | p50 | 0.544 [0.392..0.721] | 0.415 [0.383..0.819] W3/5 | 0.306 [0.252..0.413] W5/5 |
| K1-50k k1_ms | p95 | 347.571 [3.436..359.488] | 7.617 [2.797..330.232] W4/5 | 2.640 [1.821..8.035] W4/5 |
| K1-50k k1_ms | p99 | 374.549 [7.625..383.739] | 11.978 [4.065..359.075] W4/5 | 326.191 [4.024..358.265] W4/5 |
| K2 k2_ms | p50 | 0.488 [0.405..1.154] | 0.484 [0.294..0.793] W2/5 | 0.153 [0.134..0.208] W5/5 |
| K2 k2_ms | p95 | 6.118 [5.589..8.295] | 3.845 [2.809..6.585] W5/5 | 1.620 [1.146..2.205] W5/5 |
| K2 k2_ms | p99 | 10.563 [8.374..14.151] | 6.484 [5.008..9.419] W5/5 | 3.429 [3.028..4.641] W5/5 |
| K2s k2s_ms | p50 | 0.506 [0.487..0.696] | 0.497 [0.347..0.528] W3/5 | 0.145 [0.130..0.256] W5/5 |
| K2s k2s_ms | p95 | 5.154 [4.627..6.114] | 4.348 [2.526..6.475] W4/5 | 1.969 [1.266..2.839] W5/5 |
| K2s k2s_ms | p99 | 8.873 [8.418..9.556] | 7.474 [5.090..10.315] W4/5 | 4.728 [2.948..7.141] W5/5 |
| K3 k3_ms | p50 | 0.651 [0.382..0.918] | 0.442 [0.378..0.595] W5/5 | 0.172 [0.155..0.200] W5/5 |
| K3 k3_ms | p95 | 7.003 [3.421..13.277] | 4.611 [3.779..5.995] W4/5 | 3.213 [2.504..4.200] W5/5 |
| K3 k3_ms | p99 | 10.864 [6.068..19.985] | 7.663 [6.154..9.684] W4/5 | 8.074 [7.297..8.515] W4/5 |
| R1 r1_ms | p50 | 0.495 [0.427..0.632] | 0.547 [0.405..1.742] W2/5 | 0.292 [0.218..0.423] W5/5 |
| R1 r1_ms | p95 | 6.593 [2.806..6.919] | 3.761 [2.875..8.598] W2/5 | 1.867 [1.512..3.573] W4/5 |
| R1 r1_ms | p99 | 9.626 [5.543..11.600] | 6.545 [5.020..14.181] W2/5 | 3.220 [2.951..14.406] W4/5 |
| R1s r1s_ms | p50 | 0.478 [0.425..1.223] | 0.609 [0.378..1.143] W2/5 | 0.303 [0.214..0.369] W5/5 |
| R1s r1s_ms | p95 | 5.463 [2.751..8.854] | 5.942 [2.064..9.419] W2/5 | 2.512 [1.613..3.440] W4/5 |
| R1s r1s_ms | p99 | 9.506 [4.413..12.788] | 9.449 [4.471..15.503] W3/5 | 5.701 [3.001..14.569] W4/5 |
| A1 a1_ms | p50 | 10.153 [7.030..12.888] | 8.309 [7.194..11.542] W4/5 | 10.501 [8.281..11.176] W3/5 |
| A1 a1_ms | p95 | 17.500 [12.569..21.579] | 15.468 [13.075..18.730] W4/5 | 18.394 [16.396..22.400] W1/5 |
| A1 a1_ms | p99 | 23.773 [17.426..29.247] | 20.722 [17.588..26.540] W4/5 | 44.626 [43.562..47.270] W0/5 |

Lost (silent loss, max over rounds): L1: lanes=80, lanes-rt=36, single=33; L2: lanes=0, lanes-rt=0, single=1; K1: lanes=2, lanes-rt=1, single=8; L3-50k: lanes=0, lanes-rt=0, single=0; L2-50k: lanes=39, lanes-rt=0, single=50; K1-50k: lanes=5, lanes-rt=2, single=7; K2: lanes=0, lanes-rt=0, single=0; K2s: lanes=0, lanes-rt=0, single=3; K3: lanes=0, lanes-rt=0, single=0; R1: lanes=0, lanes-rt=0, single=0; R1s: lanes=0, lanes-rt=0, single=0; A1: lanes=1, lanes-rt=1, single=1

Superseded (info; sum over rounds): L3-50k: lanes=1083, lanes-rt=542, single=1130; K2: lanes=0, lanes-rt=0, single=61; K2s: lanes=21, lanes-rt=1, single=9; K3: lanes=87, lanes-rt=0, single=74; R1: lanes=9, lanes-rt=0, single=15; R1s: lanes=11, lanes-rt=0, single=8

Late (info; sum over rounds): K2: lanes=3, lanes-rt=6, single=7; K2s: lanes=7, lanes-rt=8, single=7; K3: lanes=0, lanes-rt=0, single=0

Storm achieved rate (ev/s, median): L3-50k: lanes=43953, lanes-rt=24668, single=34285; L2-50k: lanes=49765, lanes-rt=28631, single=33863; K1-50k: lanes=46394, lanes-rt=33257, single=36916

## Reader CPU seconds per run (user+sys, all threads; same scenario set per run)

- `sat`: lanes=31.7 [23.8..35.9], lanes-rt=31.1 [28.5..47.4], single=26.3 [20.6..27.7]

## Real-time grants

- `sat` / `lanes`: 60 reader starts, all-lanes-RT in 0, grants {'none': 240}, status ['not requested'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []
- `sat` / `lanes-rt`: 60 reader starts, all-lanes-RT in 60, grants {'rr:15': 60, 'rr:12': 120, 'rr:13': 60}, status ['rtkit available'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []
- `sat` / `single`: 60 reader starts, all-lanes-RT in 0, grants {}, status ['not requested'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []

## Load per run

- `sat` / `lanes` / r1: before 39.78 36.29 24.61 10/3607 3351626 (host CPU busy 47 %), with hog 38.75 36.13 24.62 25/3536 3352245, governor performance , exit 0
- `sat` / `lanes` / r2: before 44.45 38.88 31.64 8/3236 3433925 (host CPU busy 38 %), with hog 44.45 38.88 31.64 32/3501 3434729, governor performance , exit 0
- `sat` / `lanes` / r3: before 69.02 60.35 51.09 41/3358 3665547 (host CPU busy 99 %), with hog 68.62 60.41 51.16 59/3428 3665661, governor performance , exit 0
- `sat` / `lanes` / r4: before 78.91 71.89 62.61 53/3325 3708010 (host CPU busy 100 %), with hog 78.92 72.01 62.70 71/3391 3708329, governor performance , exit 0
- `sat` / `lanes` / r5: before 63.74 61.64 60.24 36/3184 3751077 (host CPU busy 100 %), with hog 63.84 61.69 60.26 52/3191 3751120, governor performance , exit 0
- `sat` / `lanes-rt` / r1: before 31.57 34.50 27.60 13/3298 3389929 (host CPU busy 14 %), with hog 31.21 34.38 27.60 22/3355 3390032, governor performance , exit 0
- `sat` / `lanes-rt` / r2: before 37.28 35.57 32.19 23/3490 3483854 (host CPU busy 99 %), with hog 35.82 35.30 32.12 20/3505 3484016, governor performance , exit 0
- `sat` / `lanes-rt` / r3: before 47.98 44.76 39.14 20/3593 3583147 (host CPU busy 68 %), with hog 47.83 44.78 39.17 30/3288 3583605, governor performance , exit 0
- `sat` / `lanes-rt` / r4: before 57.80 56.79 58.54 43/3441 3724785 (host CPU busy 100 %), with hog 57.80 56.79 58.54 53/3452 3724843, governor performance , exit 0
- `sat` / `lanes-rt` / r5: before 76.74 70.57 64.51 46/3475 3773834 (host CPU busy 99 %), with hog 76.74 70.57 64.51 56/3522 3774673, governor performance , exit 0
- `sat` / `single` / r1: before 15.31 14.78 13.96 34/3523 3309689 (host CPU busy 96 %), with hog 17.76 15.29 14.14 28/3251 3310267, governor performance , exit 0
- `sat` / `single` / r2: before 39.89 39.93 35.23 30/3529 3528679 (host CPU busy 92 %), with hog 39.58 39.87 35.24 38/3599 3529893, governor performance , exit 0
- `sat` / `single` / r3: before 52.49 57.70 47.21 26/3520 3622719 (host CPU busy 97 %), with hog 51.09 57.32 47.14 32/3543 3623751, governor performance , exit 0
- `sat` / `single` / r4: before 67.73 65.30 56.90 37/3395 3687133 (host CPU busy 100 %), with hog 67.51 65.30 56.95 58/3403 3687178, governor performance , exit 0
- `sat` / `single` / r5: before 65.71 66.86 64.75 40/3440 3789813 (host CPU busy 99 %), with hog 63.73 66.43 64.62 55/3563 3790164, governor performance , exit 0
