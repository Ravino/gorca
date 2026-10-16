# B-5 judge summary: auth-idle

```
label=auth-idle
utc=2026-10-16T10:36:28Z
git_rev=1993580 dirty=2
kernel=7.1.8+deb13-rt-amd64
cpus=16
governor=performance 
loadavg_start=74.95 68.65 65.86 49/3336 3818845
rounds=4 arms=single,lanes,lanes-rt conds=idle only=L1,L2,K1,L3-50k,L2-50k,K1-50k,K2,K2s,K3,R1,R1s,A1 quick=no
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
  load: 74.95 68.65 65.86 45/3360 3818904
loadavg_end=9.05 17.66 28.77 4/3311 4045284
```

## Condition `idle`

Value = median over rounds [min..max]; `W` = rounds in which the arm was lower than `single` in the same round / rounds.

| series | pct | single | lanes | lanes-rt |
|---|---|---|---|---|
| L1 l1_ms | p50 | 5.416 [5.283..5.552] | 5.497 [5.391..5.647] W0/4 | 5.230 [5.161..5.283] W4/4 |
| L1 l1_ms | p95 | 8.551 [7.268..9.662] | 8.351 [7.114..9.619] W2/4 | 6.257 [5.254..7.192] W4/4 |
| L1 l1_ms | p99 | 11.655 [9.579..83.874] | 10.495 [8.898..12.157] W3/4 | 8.034 [5.317..9.173] W4/4 |
| L2 l2_ms | p50 | 0.619 [0.548..0.675] | 0.415 [0.359..0.526] W4/4 | 0.314 [0.162..0.331] W4/4 |
| L2 l2_ms | p95 | 4.144 [2.209..6.200] | 2.802 [2.269..4.206] W3/4 | 3.197 [0.272..4.360] W4/4 |
| L2 l2_ms | p99 | 8.707 [3.688..10.297] | 5.018 [3.711..7.568] W3/4 | 4.626 [0.317..6.773] W4/4 |
| K1 k1_ms | p50 | 0.444 [0.362..0.809] | 0.320 [0.185..0.876] W3/4 | 0.213 [0.151..0.259] W4/4 |
| K1 k1_ms | p95 | 353.550 [9.275..665.823] | 4.271 [1.486..6.057] W4/4 | 1.075 [0.244..1.874] W4/4 |
| K1 k1_ms | p99 | 571.695 [358.300..1019.905] | 6.735 [2.960..10.381] W4/4 | 2.752 [0.290..158.395] W4/4 |
| L3-50k l1_ms | p50 | 8.138 [4.747..10.143] | 7.710 [6.269..10.471] W2/4 | 7.223 [5.142..8.954] W3/4 |
| L3-50k l1_ms | p95 | 27.294 [16.281..30.332] | 25.909 [13.026..45.653] W2/4 | 15.296 [5.268..19.286] W4/4 |
| L3-50k l1_ms | p99 | 135.739 [27.101..672.215] | 45.669 [20.021..77.259] W3/4 | 21.393 [5.409..23.864] W4/4 |
| L2-50k l2_ms | p50 | 1.943 [1.249..4.971] | 2.313 [1.246..4.861] W2/4 | 1.807 [0.130..2.730] W3/4 |
| L2-50k l2_ms | p95 | 38.258 [5.564..90.657] | 42.017 [5.096..164.673] W1/4 | 7.354 [0.236..11.785] W3/4 |
| L2-50k l2_ms | p99 | 265.490 [10.573..521.875] | 164.072 [8.578..418.586] W3/4 | 10.927 [0.301..17.570] W4/4 |
| K1-50k k1_ms | p50 | 0.255 [0.209..0.467] | 0.330 [0.202..0.424] W2/4 | 0.174 [0.142..0.339] W4/4 |
| K1-50k k1_ms | p95 | 3.057 [2.121..333.360] | 164.534 [2.134..325.077] W1/4 | 0.976 [0.248..2.392] W4/4 |
| K1-50k k1_ms | p99 | 5.865 [5.358..362.835] | 342.889 [4.039..354.702] W2/4 | 2.747 [0.302..5.212] W4/4 |
| K2 k2_ms | p50 | 0.305 [0.232..0.481] | 0.408 [0.284..0.540] W2/4 | 0.118 [0.114..0.183] W4/4 |
| K2 k2_ms | p95 | 2.930 [2.376..5.104] | 3.761 [2.663..5.582] W1/4 | 1.515 [0.188..2.464] W4/4 |
| K2 k2_ms | p99 | 5.766 [3.510..7.854] | 6.276 [4.196..7.806] W2/4 | 2.787 [0.225..5.321] W4/4 |
| K2s k2s_ms | p50 | 0.288 [0.213..0.373] | 0.475 [0.268..1.137] W0/4 | 0.117 [0.106..0.191] W4/4 |
| K2s k2s_ms | p95 | 3.260 [2.418..4.474] | 4.822 [2.070..6.689] W1/4 | 0.357 [0.191..2.918] W4/4 |
| K2s k2s_ms | p99 | 7.044 [4.124..8.198] | 8.811 [3.870..10.373] W1/4 | 1.318 [0.247..5.489] W4/4 |
| K3 k3_ms | p50 | 0.353 [0.320..0.420] | 0.472 [0.343..0.727] W0/4 | 0.180 [0.139..0.190] W4/4 |
| K3 k3_ms | p95 | 5.134 [3.861..5.688] | 4.890 [3.778..5.770] W3/4 | 1.496 [0.732..4.286] W4/4 |
| K3 k3_ms | p99 | 7.889 [6.393..9.178] | 7.948 [6.584..9.017] W2/4 | 2.552 [1.112..10.889] W3/4 |
| R1 r1_ms | p50 | 0.290 [0.261..0.364] | 0.609 [0.399..0.637] W0/4 | 0.209 [0.145..0.316] W3/4 |
| R1 r1_ms | p95 | 3.436 [1.628..4.673] | 4.692 [3.408..6.414] W0/4 | 0.474 [0.240..2.010] W4/4 |
| R1 r1_ms | p99 | 5.349 [3.602..6.816] | 7.489 [5.549..10.195] W0/4 | 2.191 [0.322..4.161] W4/4 |
| R1s r1s_ms | p50 | 0.342 [0.325..0.435] | 0.495 [0.294..0.630] W1/4 | 0.199 [0.149..0.350] W4/4 |
| R1s r1s_ms | p95 | 2.899 [2.402..3.858] | 5.193 [2.351..6.649] W1/4 | 1.146 [0.279..2.297] W4/4 |
| R1s r1s_ms | p99 | 5.923 [4.317..6.984] | 8.377 [4.207..11.943] W1/4 | 3.002 [0.352..4.086] W4/4 |
| A1 a1_ms | p50 | 6.853 [6.576..10.531] | 7.644 [4.319..8.959] W2/4 | 5.776 [3.592..10.510] W3/4 |
| A1 a1_ms | p95 | 13.505 [12.537..17.261] | 13.487 [9.687..15.216] W3/4 | 10.784 [5.172..15.872] W3/4 |
| A1 a1_ms | p99 | 20.607 [16.593..29.860] | 18.693 [15.324..22.974] W3/4 | 13.118 [5.733..41.774] W3/4 |

Lost (silent loss, max over rounds): L1: lanes=15, lanes-rt=68, single=94; L2: lanes=0, lanes-rt=0, single=3; K1: lanes=0, lanes-rt=0, single=27; L3-50k: lanes=0, lanes-rt=0, single=0; L2-50k: lanes=3, lanes-rt=1, single=4; K1-50k: lanes=5, lanes-rt=0, single=4; K2: lanes=0, lanes-rt=0, single=0; K2s: lanes=0, lanes-rt=0, single=33; K3: lanes=0, lanes-rt=0, single=0; R1: lanes=0, lanes-rt=0, single=0; R1s: lanes=0, lanes-rt=0, single=0; A1: lanes=1, lanes-rt=0, single=3

Superseded (info; sum over rounds): L3-50k: lanes=502, lanes-rt=669, single=761; K2: lanes=1, lanes-rt=0, single=60; K2s: lanes=6, lanes-rt=1, single=107; K3: lanes=84, lanes-rt=0, single=34; R1: lanes=12, lanes-rt=0, single=5; R1s: lanes=8, lanes-rt=0, single=10

Late (info; sum over rounds): K2: lanes=1, lanes-rt=9, single=0; K2s: lanes=5, lanes-rt=1, single=48; K3: lanes=0, lanes-rt=0, single=0

Storm achieved rate (ev/s, median): L3-50k: lanes=40134, lanes-rt=25814, single=42679; L2-50k: lanes=45477, lanes-rt=35729, single=46073; K1-50k: lanes=46013, lanes-rt=33996, single=45830

## Reader CPU seconds per run (user+sys, all threads; same scenario set per run)

- `idle`: lanes=30.5 [22.8..35.3], lanes-rt=39.0 [31.3..55.6], single=31.6 [23.3..42.5]

## Real-time grants

- `idle` / `lanes`: 48 reader starts, all-lanes-RT in 0, grants {'none': 192}, status ['not requested'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []
- `idle` / `lanes-rt`: 48 reader starts, all-lanes-RT in 48, grants {'rr:15': 48, 'rr:12': 96, 'rr:13': 48}, status ['rtkit available'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []
- `idle` / `single`: 48 reader starts, all-lanes-RT in 0, grants {}, status ['not requested'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []

## Load per run

- `idle` / `lanes` / r1: before 54.84 60.61 63.38 46/3071 3833762 (host CPU busy 100 %), with hog 54.84 60.61 63.38 53/3083 3833806, governor performance , exit 0
- `idle` / `lanes` / r2: before 35.76 52.11 57.79 46/3053 3866751 (host CPU busy 99 %), with hog 35.76 52.11 57.79 46/2982 3866785, governor performance , exit 0
- `idle` / `lanes` / r3: before 28.99 36.22 45.08 20/3055 3979913 (host CPU busy 99 %), with hog 28.99 36.22 45.08 17/3069 3979993, governor performance , exit 0
- `idle` / `lanes` / r4: before 31.28 32.43 38.21 29/2967 4020357 (host CPU busy 100 %), with hog 31.28 32.43 38.21 21/2915 4020364, governor performance , exit 0
- `idle` / `lanes-rt` / r1: before 52.13 54.99 59.89 46/3191 3851266 (host CPU busy 99 %), with hog 52.13 54.99 59.89 40/3192 3851276, governor performance , exit 0
- `idle` / `lanes-rt` / r2: before 49.92 51.68 55.82 47/2945 3890009 (host CPU busy 100 %), with hog 49.12 51.49 55.73 43/2946 3890053, governor performance , exit 0
- `idle` / `lanes-rt` / r3: before 52.70 53.26 54.90 58/3336 3917777 (host CPU busy 100 %), with hog 52.70 53.26 54.90 47/3335 3917812, governor performance , exit 0
- `idle` / `lanes-rt` / r4: before 18.87 29.68 35.61 5/3034 4032601 (host CPU busy 9 %), with hog 18.72 29.47 35.51 2/2948 4032608, governor performance , exit 0
- `idle` / `single` / r1: before 74.95 68.65 65.86 47/3357 3818906 (host CPU busy 100 %), with hog 73.76 68.51 65.83 48/3355 3818967, governor performance , exit 0
- `idle` / `single` / r2: before 61.75 56.02 56.23 63/3055 3901041 (host CPU busy 100 %), with hog 61.75 56.02 56.23 55/3054 3901056, governor performance , exit 0
- `idle` / `single` / r3: before 26.41 39.73 49.05 11/3008 3952373 (host CPU busy 45 %), with hog 26.54 39.54 48.93 6/3002 3952644, governor performance , exit 0
- `idle` / `single` / r4: before 30.37 33.14 40.93 23/3286 4000265 (host CPU busy 100 %), with hog 30.37 33.14 40.93 20/3284 4000283, governor performance , exit 0
