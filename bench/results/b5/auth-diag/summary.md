# B-5 judge summary: auth-diag

```
label=auth-diag
utc=2026-10-16T11:45:31Z
git_rev=1993580 dirty=2
kernel=7.1.8+deb13-rt-amd64
cpus=16
governor=performance 
loadavg_start=9.05 17.66 28.77 2/3311 4045313
rounds=3 arms=single,lanes-rt,lanes-rt-nopin,lanes-pin conds=sat only=L1,L2,K1,L3-50k,L2-50k,K1-50k,K2s,R1s quick=no
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
  load: 9.05 17.66 28.77 3/3313 4045342
loadavg_end=28.74 28.58 28.73 4/3264 4063093
```

## Condition `sat`

Value = median over rounds [min..max]; `W` = rounds in which the arm was lower than `single` in the same round / rounds.

| series | pct | single | lanes-rt | lanes-rt-nopin | lanes-pin |
|---|---|---|---|---|---|
| L1 l1_ms | p50 | 5.345 [5.345..5.345] | 5.312 [5.293..5.330] W1/1 | 5.307 [5.307..5.307] W1/1 | 5.284 [5.284..5.284] W1/1 |
| L1 l1_ms | p95 | 7.286 [7.286..7.286] | 7.210 [7.170..7.250] W1/1 | 6.599 [6.027..7.170] W1/1 | 7.243 [7.243..7.243] W1/1 |
| L1 l1_ms | p99 | 9.685 [9.685..9.685] | 11.593 [9.369..13.817] W0/1 | 8.302 [8.045..8.560] W1/1 | 8.439 [8.439..8.439] W1/1 |
| L2 l2_ms | p50 | 0.504 [0.504..0.504] | 0.329 [0.318..0.340] W1/1 | 0.335 [0.328..0.342] W1/1 | 0.349 [0.349..0.349] W1/1 |
| L2 l2_ms | p95 | 3.097 [3.097..3.097] | 2.674 [2.280..3.069] W1/1 | 2.176 [1.515..2.838] W1/1 | 2.941 [2.941..2.941] W1/1 |
| L2 l2_ms | p99 | 4.300 [4.300..4.300] | 4.191 [4.007..4.376] W1/1 | 4.027 [3.049..5.005] W0/1 | 4.995 [4.995..4.995] W0/1 |
| K1 k1_ms | p50 | 0.957 [0.957..0.957] | 0.360 [0.293..0.426] W1/1 | 0.283 [0.233..0.333] W1/1 | 0.288 [0.288..0.288] W1/1 |
| K1 k1_ms | p95 | 3.613 [3.613..3.613] | 2.014 [1.635..2.394] W1/1 | 1.757 [1.723..1.792] W1/1 | 2.342 [2.342..2.342] W1/1 |
| K1 k1_ms | p99 | 5.004 [5.004..5.004] | 3.331 [3.087..3.576] W1/1 | 3.154 [3.028..3.281] W1/1 | 3.756 [3.756..3.756] W1/1 |
| L3-50k l1_ms | p50 | 6.106 [6.106..6.106] | 5.784 [5.719..5.848] W1/1 | 5.784 [5.773..5.795] W1/1 | 6.150 [6.150..6.150] W0/1 |
| L3-50k l1_ms | p95 | 11.388 [11.388..11.388] | 9.320 [9.087..9.553] W1/1 | 8.202 [8.005..8.398] W1/1 | 10.472 [10.472..10.472] W1/1 |
| L3-50k l1_ms | p99 | 16.034 [16.034..16.034] | 12.621 [12.306..12.936] W1/1 | 11.750 [10.190..13.311] W1/1 | 14.589 [14.589..14.589] W1/1 |
| L2-50k l2_ms | p50 | 0.860 [0.860..0.860] | 0.725 [0.667..0.784] W1/1 | 0.915 [0.864..0.965] W0/1 | 1.407 [1.407..1.407] W0/1 |
| L2-50k l2_ms | p95 | 3.663 [3.663..3.663] | 3.316 [3.290..3.342] W1/1 | 3.258 [3.198..3.318] W1/1 | 5.954 [5.954..5.954] W0/1 |
| L2-50k l2_ms | p99 | 6.258 [6.258..6.258] | 5.686 [5.307..6.065] W1/1 | 5.279 [4.362..6.196] W1/1 | 24.749 [24.749..24.749] W0/1 |
| K1-50k k1_ms | p50 | 0.246 [0.246..0.246] | 0.277 [0.215..0.339] W1/1 | 0.225 [0.219..0.232] W1/1 | 0.323 [0.323..0.323] W0/1 |
| K1-50k k1_ms | p95 | 1.449 [1.449..1.449] | 1.894 [1.731..2.057] W0/1 | 1.021 [0.626..1.416] W1/1 | 344.363 [344.363..344.363] W0/1 |
| K1-50k k1_ms | p99 | 2.897 [2.897..2.897] | 3.214 [3.102..3.326] W0/1 | 2.268 [1.821..2.715] W1/1 | 684.426 [684.426..684.426] W0/1 |
| K2s k2s_ms | p50 | 0.250 [0.250..0.250] | 0.165 [0.099..0.230] W1/1 | 0.211 [0.174..0.248] W1/1 | 0.184 [0.184..0.184] W1/1 |
| K2s k2s_ms | p95 | 2.564 [2.564..2.564] | 1.192 [0.344..2.040] W1/1 | 1.590 [1.270..1.910] W1/1 | 2.868 [2.868..2.868] W0/1 |
| K2s k2s_ms | p99 | 5.728 [5.728..5.728] | 2.383 [1.710..3.055] W1/1 | 3.221 [3.095..3.347] W1/1 | 4.504 [4.504..4.504] W1/1 |
| R1s r1s_ms | p50 | 0.384 [0.384..0.384] | 0.207 [0.198..0.217] W1/1 | 0.252 [0.247..0.258] W1/1 | 0.385 [0.385..0.385] W0/1 |
| R1s r1s_ms | p95 | 2.779 [2.779..2.779] | 0.831 [0.706..0.956] W1/1 | 1.129 [1.121..1.138] W1/1 | 3.135 [3.135..3.135] W0/1 |
| R1s r1s_ms | p99 | 5.473 [5.473..5.473] | 2.553 [2.452..2.654] W1/1 | 2.498 [2.430..2.567] W1/1 | 4.822 [4.822..4.822] W1/1 |

Lost (silent loss, max over rounds): L1: lanes-pin=0, lanes-rt=0, lanes-rt-nopin=0, single=0; L2: lanes-pin=0, lanes-rt=0, lanes-rt-nopin=0, single=0; K1: lanes-pin=0, lanes-rt=0, lanes-rt-nopin=0, single=0; L3-50k: lanes-pin=0, lanes-rt=0, lanes-rt-nopin=0, single=0; L2-50k: lanes-pin=0, lanes-rt=0, lanes-rt-nopin=0, single=0; K1-50k: lanes-pin=35, lanes-rt=0, lanes-rt-nopin=0, single=0; K2s: lanes-pin=0, lanes-rt=0, lanes-rt-nopin=0, single=0; R1s: lanes-pin=0, lanes-rt=0, lanes-rt-nopin=0, single=0

Superseded (info; sum over rounds): L3-50k: lanes-pin=17, lanes-rt=43, lanes-rt-nopin=9, single=16; K2s: lanes-pin=0, lanes-rt=0, lanes-rt-nopin=0, single=1; R1s: lanes-pin=1, lanes-rt=0, lanes-rt-nopin=0, single=1

Late (info; sum over rounds): K2s: lanes-pin=0, lanes-rt=0, lanes-rt-nopin=2, single=3

Storm achieved rate (ev/s, median): L3-50k: lanes-pin=49987, lanes-rt=48419, lanes-rt-nopin=47082, single=49980; L2-50k: lanes-pin=49422, lanes-rt=50000, lanes-rt-nopin=48668, single=50000; K1-50k: lanes-pin=49618, lanes-rt=47366, lanes-rt-nopin=49869, single=49997

## Reader CPU seconds per run (user+sys, all threads; same scenario set per run)

- `sat`: lanes-pin=37.3 [37.3..37.3], lanes-rt=50.7 [50.2..51.2], lanes-rt-nopin=53.1 [49.9..56.2], single=32.1 [32.1..32.1]

## Real-time grants

- `sat` / `lanes-pin`: 8 reader starts, all-lanes-RT in 0, grants {'none': 32}, status ['not requested'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []
- `sat` / `lanes-rt`: 16 reader starts, all-lanes-RT in 16, grants {'rr:15': 16, 'rr:12': 32, 'rr:13': 16}, status ['rtkit available'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []
- `sat` / `lanes-rt-nopin`: 16 reader starts, all-lanes-RT in 16, grants {'rr:15': 16, 'rr:12': 32, 'rr:13': 16}, status ['rtkit available'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []
- `sat` / `single`: 8 reader starts, all-lanes-RT in 0, grants {}, status ['not requested'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []

## Load per run

- `sat` / `lanes-pin` / r1: before 29.57 27.00 28.58 4/2927 4053258 (host CPU busy 8 %), with hog 28.72 26.87 28.53 19/2942 4053300, governor performance , exit 0
- `sat` / `lanes-pin` / r2: before 29.16 28.61 28.74 5/3269 4061991 (host CPU busy 7 %), with hog 29.16 28.61 28.74 22/3275 4062054, governor performance , exit 1
- `sat` / `lanes-pin` / r3: before 29.95 28.80 28.80 4/3249 4062532 (host CPU busy 7 %), with hog 29.95 28.80 28.80 23/3268 4062590, governor performance , exit 1
- `sat` / `lanes-rt` / r1: before 28.08 22.02 28.40 4/3232 4047531 (host CPU busy 16 %), with hog 28.23 22.15 28.41 24/3252 4047576, governor performance , exit 0
- `sat` / `lanes-rt` / r2: before 30.73 28.18 28.69 4/3291 4056376 (host CPU busy 9 %), with hog 30.75 28.23 28.71 18/3308 4056419, governor performance , exit 0
- `sat` / `lanes-rt` / r3: before 28.80 28.59 28.73 3/3272 4062928 (host CPU busy 7 %), with hog 28.80 28.59 28.73 19/3289 4062965, governor performance , exit 1
- `sat` / `lanes-rt-nopin` / r1: before 28.03 25.11 28.46 3/2832 4050122 (host CPU busy 9 %), with hog 28.03 25.11 28.46 18/2832 4050168, governor performance , exit 0
- `sat` / `lanes-rt-nopin` / r2: before 27.43 28.43 28.74 3/3302 4059444 (host CPU busy 7 %), with hog 27.79 28.49 28.76 18/3320 4059482, governor performance , exit 0
- `sat` / `lanes-rt-nopin` / r3: before 29.16 28.63 28.74 5/3258 4062363 (host CPU busy 9 %), with hog 29.16 28.63 28.74 24/3267 4062400, governor performance , exit 1
- `sat` / `single` / r1: before 9.05 17.66 28.77 4/3311 4045344 (host CPU busy 10 %), with hog 9.44 17.60 28.69 20/3339 4045408, governor performance , exit 0
- `sat` / `single` / r2: before 28.91 28.57 28.73 8/3261 4062200 (host CPU busy 7 %), with hog 28.91 28.57 28.73 22/3274 4062238, governor performance , exit 1
- `sat` / `single` / r3: before 29.31 28.69 28.76 3/3267 4062736 (host CPU busy 8 %), with hog 29.31 28.69 28.76 25/3287 4062798, governor performance , exit 1
