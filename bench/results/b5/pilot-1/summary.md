# B-5 judge summary: pilot-1

```
label=pilot-1
utc=2026-10-14T23:40:46Z
git_rev=932d611 dirty=13
kernel=7.1.8+deb13-rt-amd64
cpus=16
governor=performance 
loadavg_start=16.52 13.85 12.32 7/3231 178218
rounds=3 arms=single,lanes,lanes-rt conds=idle,sat only=L1,L2,K1,L3-50k,L2-50k,K1-50k quick=--quick
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
  load: 16.52 13.85 12.32 6/3233 178247
loadavg_end=27.77 22.82 17.69 3/3106 248713
```

## Condition `idle`

Value = median over rounds [min..max]; `W` = rounds in which the arm was lower than `single` in the same round / rounds.

| series | pct | single | lanes | lanes-rt |
|---|---|---|---|---|
| L1 l1_ms | p50 | 0.602 [0.582..0.622] | 0.593 [0.543..0.608] W3/3 | 0.554 [0.547..0.769] W2/3 |
| L1 l1_ms | p95 | 0.918 [0.821..1.015] | 0.857 [0.784..0.923] W2/3 | 0.904 [0.830..2.159] W1/3 |
| L1 l1_ms | p99 | 1.108 [1.092..1.289] | 1.004 [0.907..1.126] W3/3 | 1.032 [1.032..5.664] W2/3 |
| L2 l2_ms | p50 | 0.215 [0.209..0.238] | 0.239 [0.203..0.291] W1/3 | 0.200 [0.195..0.211] W3/3 |
| L2 l2_ms | p95 | 0.305 [0.287..0.353] | 0.333 [0.293..0.479] W1/3 | 0.288 [0.269..0.405] W1/3 |
| L2 l2_ms | p99 | 0.331 [0.311..0.414] | 0.511 [0.349..1.712] W0/3 | 0.320 [0.318..1.038] W1/3 |
| K1 k1_ms | p50 | 0.244 [0.191..0.257] | 0.218 [0.199..0.248] W1/3 | 0.171 [0.162..0.276] W2/3 |
| K1 k1_ms | p95 | 0.361 [0.294..0.517] | 0.329 [0.314..0.491] W2/3 | 0.267 [0.243..0.506] W3/3 |
| K1 k1_ms | p99 | 0.482 [0.335..0.661] | 0.387 [0.373..1.298] W1/3 | 0.326 [0.326..2.936] W2/3 |
| L3-50k l1_ms | p50 | 0.537 [0.516..0.551] | 0.584 [0.492..0.668] W1/3 | 0.540 [0.529..0.549] W1/3 |
| L3-50k l1_ms | p95 | 0.845 [0.805..1.111] | 0.952 [0.745..2.378] W1/3 | 1.384 [1.185..1.636] W0/3 |
| L3-50k l1_ms | p99 | 1.116 [0.940..1.307] | 1.284 [1.252..6.398] W0/3 | 1.959 [1.621..2.640] W0/3 |
| L2-50k l2_ms | p50 | 0.159 [0.156..0.174] | 0.177 [0.158..0.550] W0/3 | 0.132 [0.125..0.160] W2/3 |
| L2-50k l2_ms | p95 | 0.327 [0.241..0.436] | 0.242 [0.230..3.258] W2/3 | 0.203 [0.188..0.237] W3/3 |
| L2-50k l2_ms | p99 | 0.808 [0.272..2.561] | 0.283 [0.276..4.145] W1/3 | 0.288 [0.223..0.307] W3/3 |
| K1-50k k1_ms | p50 | 0.203 [0.195..0.227] | 0.276 [0.177..0.346] W1/3 | 0.157 [0.138..0.164] W3/3 |
| K1-50k k1_ms | p95 | 0.330 [0.324..0.344] | 0.381 [0.294..1.031] W1/3 | 0.280 [0.201..0.302] W3/3 |
| K1-50k k1_ms | p99 | 0.429 [0.417..0.438] | 0.430 [0.339..3.417] W2/3 | 0.347 [0.254..0.431] W3/3 |

Lost probes (max over rounds): L1: lanes=0, lanes-rt=0, single=0; L2: lanes=0, lanes-rt=0, single=0; K1: lanes=0, lanes-rt=0, single=0; L3-50k: lanes=0, lanes-rt=0, single=0; L2-50k: lanes=0, lanes-rt=0, single=0; K1-50k: lanes=0, lanes-rt=0, single=0

Storm achieved rate (ev/s, median): L3-50k: lanes=50000, lanes-rt=49998, single=49997; L2-50k: lanes=49998, lanes-rt=49999, single=49999; K1-50k: lanes=49998, lanes-rt=50000, single=49998

## Condition `sat`

Value = median over rounds [min..max]; `W` = rounds in which the arm was lower than `single` in the same round / rounds.

| series | pct | single | lanes | lanes-rt |
|---|---|---|---|---|
| L1 l1_ms | p50 | 1.573 [1.260..1.616] | 1.188 [0.758..2.047] W2/3 | 1.135 [0.758..1.378] W3/3 |
| L1 l1_ms | p95 | 4.864 [4.213..5.377] | 5.281 [3.224..5.480] W1/3 | 3.901 [3.709..5.445] W2/3 |
| L1 l1_ms | p99 | 7.914 [7.508..8.498] | 6.480 [5.675..8.410] W2/3 | 6.531 [6.507..7.784] W3/3 |
| L2 l2_ms | p50 | 0.592 [0.524..0.800] | 0.427 [0.394..0.477] W3/3 | 0.365 [0.350..0.467] W3/3 |
| L2 l2_ms | p95 | 3.489 [2.912..3.653] | 1.881 [0.844..4.905] W2/3 | 2.620 [1.973..3.152] W2/3 |
| L2 l2_ms | p99 | 5.103 [4.455..6.141] | 4.038 [2.707..6.392] W2/3 | 4.866 [3.066..9.362] W2/3 |
| K1 k1_ms | p50 | 0.383 [0.373..0.406] | 0.378 [0.227..0.416] W2/3 | 0.350 [0.305..0.364] W3/3 |
| K1 k1_ms | p95 | 2.551 [2.331..2.781] | 2.884 [2.452..3.266] W1/3 | 1.911 [1.610..2.075] W3/3 |
| K1 k1_ms | p99 | 3.238 [3.146..5.274] | 4.438 [3.060..5.920] W2/3 | 2.850 [2.237..3.264] W3/3 |
| L3-50k l1_ms | p50 | 6.204 [3.608..9.413] | 3.954 [1.963..5.324] W2/3 | 4.842 [4.837..5.413] W2/3 |
| L3-50k l1_ms | p95 | 26.761 [10.203..51.241] | 29.356 [9.278..62.030] W1/3 | 10.338 [8.963..12.412] W2/3 |
| L3-50k l1_ms | p99 | 31.943 [15.828..59.864] | 52.771 [10.728..97.588] W1/3 | 13.509 [9.763..18.218] W3/3 |
| L2-50k l2_ms | p50 | 2.310 [1.974..3.362] | 1.881 [1.631..1.907] W3/3 | 0.986 [0.917..1.017] W3/3 |
| L2-50k l2_ms | p95 | 8.775 [7.971..51.440] | 6.916 [6.336..34.394] W2/3 | 3.744 [3.399..4.075] W3/3 |
| L2-50k l2_ms | p99 | 87.315 [11.132..98.468] | 11.352 [9.885..178.842] W1/3 | 6.818 [4.015..7.230] W3/3 |
| K1-50k k1_ms | p50 | 0.378 [0.341..0.389] | 0.380 [0.329..0.413] W1/3 | 0.249 [0.244..0.493] W2/3 |
| K1-50k k1_ms | p95 | 3.451 [2.556..3.637] | 3.122 [2.543..4.764] W2/3 | 0.617 [0.446..14.253] W2/3 |
| K1-50k k1_ms | p99 | 4.527 [4.333..6.134] | 4.751 [4.167..6.320] W2/3 | 1.419 [1.108..31.586] W2/3 |

Lost probes (max over rounds): L1: lanes=0, lanes-rt=1, single=0; L2: lanes=0, lanes-rt=0, single=0; K1: lanes=0, lanes-rt=0, single=0; L3-50k: lanes=0, lanes-rt=0, single=0; L2-50k: lanes=0, lanes-rt=0, single=0; K1-50k: lanes=1, lanes-rt=0, single=0

Storm achieved rate (ev/s, median): L3-50k: lanes=49951, lanes-rt=46533, single=49710; L2-50k: lanes=49900, lanes-rt=49614, single=48886; K1-50k: lanes=49933, lanes-rt=49940, single=49735

## Real-time grants

- `idle` / `lanes`: 18 reader starts, all-lanes-RT in 0, grants {'none': 72}, status ['not requested'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []
- `idle` / `lanes-rt`: 18 reader starts, all-lanes-RT in 18, grants {'rr:15': 18, 'rr:12': 36, 'rr:13': 18}, status ['rtkit available'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []
- `idle` / `single`: 18 reader starts, all-lanes-RT in 0, grants {}, status ['not requested'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []
- `sat` / `lanes`: 18 reader starts, all-lanes-RT in 0, grants {'none': 72}, status ['not requested'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []
- `sat` / `lanes-rt`: 18 reader starts, all-lanes-RT in 18, grants {'rr:15': 18, 'rr:12': 36, 'rr:13': 18}, status ['rtkit available'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []
- `sat` / `single`: 18 reader starts, all-lanes-RT in 0, grants {}, status ['not requested'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []

## Load per run

- `idle` / `lanes` / r1: before 14.90 13.71 12.32 3/3245 179374, with hog 14.90 13.71 12.32 5/3245 179375, governor performance , exit 0
- `idle` / `lanes` / r2: before 30.71 19.65 14.69 5/3010 185332, with hog 30.71 19.65 14.69 6/3010 185333, governor performance , exit 0
- `idle` / `lanes` / r3: before 18.86 20.85 16.42 4/3167 229727, with hog 18.86 20.85 16.42 3/3167 229728, governor performance , exit 0
- `idle` / `lanes-rt` / r1: before 16.08 14.07 12.48 3/3204 180211, with hog 16.08 14.07 12.48 4/3204 180212, governor performance , exit 0
- `idle` / `lanes-rt` / r2: before 29.67 20.34 15.07 10/3321 198397, with hog 29.67 20.34 15.07 5/3321 198398, governor performance , exit 0
- `idle` / `lanes-rt` / r3: before 29.49 22.84 16.76 7/3135 220877, with hog 29.49 22.84 16.76 5/3135 220878, governor performance , exit 0
- `idle` / `single` / r1: before 16.52 13.85 12.32 8/3231 178249, with hog 16.52 13.85 12.32 8/3231 178250, governor performance , exit 0
- `idle` / `single` / r2: before 22.70 19.58 14.99 3/3056 202586, with hog 22.70 19.58 14.99 4/3056 202587, governor performance , exit 0
- `idle` / `single` / r3: before 20.63 21.32 16.45 3/3160 225493, with hog 20.63 21.32 16.45 4/3160 225494, governor performance , exit 0
- `sat` / `lanes` / r1: before 23.52 16.13 13.27 3/3202 182357, with hog 23.52 16.13 13.27 28/3220 182446, governor performance , exit 0
- `sat` / `lanes` / r2: before 19.02 18.93 14.92 7/3064 206297, with hog 19.02 18.93 14.92 21/3082 206379, governor performance , exit 0
- `sat` / `lanes` / r3: before 24.66 21.79 17.18 6/3133 243731, with hog 24.66 21.79 17.18 33/3264 244322, governor performance , exit 0
- `sat` / `lanes-rt` / r1: before 27.10 17.79 13.93 8/3037 183431, with hog 27.10 17.79 13.93 26/3055 183467, governor performance , exit 0
- `sat` / `lanes-rt` / r2: before 25.04 20.35 15.54 9/3128 209907, with hog 25.04 20.35 15.54 24/3134 210129, governor performance , exit 0
- `sat` / `lanes-rt` / r3: before 16.36 20.05 16.30 5/3191 234149, with hog 16.36 20.05 16.30 21/2988 234476, governor performance , exit 0
- `sat` / `single` / r1: before 14.72 13.94 12.48 4/3186 181181, with hog 14.72 13.94 12.48 18/3200 181263, governor performance , exit 0
- `sat` / `single` / r2: before 29.47 22.00 16.27 3/3188 215263, with hog 29.47 22.00 16.27 23/3227 215889, governor performance , exit 0
- `sat` / `single` / r3: before 21.49 20.95 16.74 7/3238 239104, with hog 21.49 20.95 16.74 20/3252 239312, governor performance , exit 0
