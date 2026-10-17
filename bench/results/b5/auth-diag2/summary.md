# B-5 judge summary: auth-diag2

```
label=auth-diag2
utc=2026-10-16T12:32:30Z
git_rev=e185a10 dirty=5
kernel=7.1.8+deb13-rt-amd64
cpus=16
governor=performance 
loadavg_start=13.70 8.53 10.70 9/3362 4093257
rounds=4 arms=single,lanes-rt,lanes-rt-nopin,lanes-pin conds=sat only=L1,L3-50k,L2-50k,K1-50k,K2s,R1s,K3,A1 quick=no
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
  load: 13.70 8.53 10.70 1/3348 4093292
loadavg_end=36.72 33.53 34.09 5/3407 69451
```

## Condition `sat`

Value = median over rounds [min..max]; `W` = rounds in which the arm was lower than `single` in the same round / rounds.

| series | pct | single | lanes-rt | lanes-rt-nopin | lanes-pin |
|---|---|---|---|---|---|
| L1 l1_ms | p50 | 5.429 [3.889..5.599] | 5.350 [5.319..5.382] W3/4 | 5.370 [5.334..5.465] W3/4 | 5.441 [5.315..6.284] W2/4 |
| L1 l1_ms | p95 | 7.602 [6.899..8.177] | 7.675 [6.864..8.392] W1/4 | 7.507 [6.963..8.679] W2/4 | 8.527 [8.224..10.442] W0/4 |
| L1 l1_ms | p99 | 10.342 [8.503..11.016] | 9.989 [8.728..33.498] W2/4 | 15.681 [8.272..23.563] W1/4 | 11.612 [10.560..13.523] W1/4 |
| L3-50k l1_ms | p50 | 7.207 [6.166..9.090] | 6.118 [5.987..7.442] W3/4 | 5.918 [5.818..6.190] W4/4 | 6.719 [6.144..7.532] W4/4 |
| L3-50k l1_ms | p95 | 26.851 [11.837..34.257] | 11.231 [10.568..18.430] W4/4 | 10.322 [7.685..15.170] W4/4 | 16.793 [10.105..95.548] W3/4 |
| L3-50k l1_ms | p99 | 48.671 [22.500..90.154] | 16.350 [13.618..25.481] W4/4 | 14.013 [9.720..26.547] W4/4 | 82.010 [13.522..287.294] W2/4 |
| L2-50k l2_ms | p50 | 2.089 [1.222..2.448] | 1.177 [0.954..1.397] W4/4 | 0.973 [0.875..1.195] W4/4 | 1.882 [1.344..2.506] W1/4 |
| L2-50k l2_ms | p95 | 27.159 [5.657..49.238] | 6.166 [3.811..6.500] W4/4 | 4.875 [3.248..6.077] W4/4 | 10.728 [4.809..21.251] W4/4 |
| L2-50k l2_ms | p99 | 108.427 [40.968..195.400] | 13.287 [6.491..17.630] W4/4 | 11.214 [4.268..19.964] W4/4 | 23.225 [7.457..68.742] W4/4 |
| K1-50k k1_ms | p50 | 0.395 [0.373..0.459] | 0.301 [0.271..0.312] W4/4 | 0.303 [0.296..0.316] W4/4 | 0.544 [0.328..328.530] W2/4 |
| K1-50k k1_ms | p95 | 2.663 [2.176..5.250] | 2.021 [1.269..2.426] W4/4 | 3.315 [1.544..342.884] W1/4 | 166.395 [3.079..685.841] W0/4 |
| K1-50k k1_ms | p99 | 166.376 [3.720..339.206] | 3.907 [2.935..5.073] W4/4 | 173.282 [3.278..374.276] W1/4 | 348.352 [4.728..996.280] W0/4 |
| K2s k2s_ms | p50 | 0.289 [0.256..0.366] | 0.154 [0.139..0.166] W4/4 | 0.160 [0.152..0.194] W4/4 | 0.308 [0.223..0.564] W2/4 |
| K2s k2s_ms | p95 | 2.643 [2.072..3.192] | 1.861 [1.656..2.650] W4/4 | 1.618 [1.144..2.351] W4/4 | 3.639 [3.479..5.381] W0/4 |
| K2s k2s_ms | p99 | 5.055 [4.091..5.735] | 3.669 [2.957..7.713] W3/4 | 3.702 [2.601..4.314] W4/4 | 6.241 [5.754..11.932] W0/4 |
| K3 k3_ms | p50 | 0.433 [0.359..0.529] | 0.180 [0.154..0.192] W4/4 | 0.165 [0.148..0.269] W4/4 | 0.485 [0.241..0.868] W2/4 |
| K3 k3_ms | p95 | 4.433 [3.858..5.647] | 3.200 [2.435..3.751] W4/4 | 2.279 [1.772..4.911] W3/4 | 5.914 [5.376..6.712] W0/4 |
| K3 k3_ms | p99 | 7.201 [6.900..8.960] | 6.444 [4.883..8.375] W3/4 | 5.270 [3.596..9.681] W3/4 | 9.352 [7.675..10.719] W0/4 |
| R1s r1s_ms | p50 | 0.415 [0.328..0.425] | 0.283 [0.230..0.298] W4/4 | 0.263 [0.250..0.378] W3/4 | 0.418 [0.372..0.784] W2/4 |
| R1s r1s_ms | p95 | 2.841 [1.826..2.980] | 1.755 [1.415..1.965] W4/4 | 1.511 [0.733..2.409] W3/4 | 3.858 [3.458..8.669] W0/4 |
| R1s r1s_ms | p99 | 5.686 [3.363..6.896] | 4.291 [3.149..6.331] W3/4 | 3.539 [2.313..9.199] W2/4 | 7.240 [5.216..16.374] W1/4 |
| A1 a1_ms | p50 | 6.517 [5.182..7.645] | 7.732 [6.992..8.523] W1/4 | 6.923 [6.022..7.351] W1/4 | 8.048 [6.858..8.093] W1/4 |
| A1 a1_ms | p95 | 12.761 [11.245..14.994] | 13.828 [11.719..15.479] W2/4 | 11.291 [10.334..13.216] W3/4 | 14.370 [12.901..21.586] W1/4 |
| A1 a1_ms | p99 | 15.659 [13.799..18.251] | 19.794 [15.381..42.984] W2/4 | 27.615 [10.947..43.281] W2/4 | 35.514 [17.281..70.095] W1/4 |

Lost (silent loss, max over rounds): L1: lanes-pin=24, lanes-rt=35, lanes-rt-nopin=95, single=22; L3-50k: lanes-pin=0, lanes-rt=0, lanes-rt-nopin=0, single=0; L2-50k: lanes-pin=1, lanes-rt=0, lanes-rt-nopin=0, single=0; K1-50k: lanes-pin=41, lanes-rt=0, lanes-rt-nopin=7, single=3; K2s: lanes-pin=4, lanes-rt=0, lanes-rt-nopin=0, single=8; K3: lanes-pin=0, lanes-rt=0, lanes-rt-nopin=0, single=0; R1s: lanes-pin=8, lanes-rt=0, lanes-rt-nopin=0, single=0; A1: lanes-pin=2, lanes-rt=2, lanes-rt-nopin=1, single=2

Superseded (info; sum over rounds): L3-50k: lanes-pin=359, lanes-rt=268, lanes-rt-nopin=418, single=490; K2s: lanes-pin=67, lanes-rt=2, lanes-rt-nopin=2, single=17; K3: lanes-pin=211, lanes-rt=2, lanes-rt-nopin=1, single=31; R1s: lanes-pin=103, lanes-rt=0, lanes-rt-nopin=0, single=5

Late (info; sum over rounds): K2s: lanes-pin=33, lanes-rt=16, lanes-rt-nopin=21, single=14; K3: lanes-pin=0, lanes-rt=0, lanes-rt-nopin=0, single=0

Storm achieved rate (ev/s, median): L3-50k: lanes-pin=48781, lanes-rt=37649, lanes-rt-nopin=41655, single=46446; L2-50k: lanes-pin=49794, lanes-rt=45866, lanes-rt-nopin=48855, single=49990; K1-50k: lanes-pin=49942, lanes-rt=49859, lanes-rt-nopin=46975, single=49994

## Reader CPU seconds per run (user+sys, all threads; same scenario set per run)

- `sat`: lanes-pin=36.8 [31.8..40.1], lanes-rt=48.1 [44.5..54.3], lanes-rt-nopin=47.4 [45.1..49.0], single=32.1 [29.4..36.5]

## Real-time grants

- `sat` / `lanes-pin`: 32 reader starts, all-lanes-RT in 0, grants {'none': 128}, status ['not requested'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []
- `sat` / `lanes-rt`: 32 reader starts, all-lanes-RT in 32, grants {'rr:15': 32, 'rr:12': 64, 'rr:13': 32}, status ['rtkit available'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []
- `sat` / `lanes-rt-nopin`: 32 reader starts, all-lanes-RT in 32, grants {'rr:15': 32, 'rr:12': 64, 'rr:13': 32}, status ['rtkit available'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []
- `sat` / `single`: 32 reader starts, all-lanes-RT in 0, grants {}, status ['not requested'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []

## Load per run

- `sat` / `lanes-pin` / r1: before 36.05 39.89 29.16 13/3420 4144919 (host CPU busy 36 %), with hog 35.32 39.68 29.15 32/3418 4145066, governor performance , exit 0
- `sat` / `lanes-pin` / r2: before 38.54 40.32 34.91 24/3011 4187767 (host CPU busy 99 %), with hog 38.54 40.32 34.91 43/3039 4187841, governor performance , exit 0
- `sat` / `lanes-pin` / r3: before 31.52 35.23 35.40 6/2957 11833 (host CPU busy 42 %), with hog 30.44 34.95 35.30 32/2975 11927, governor performance , exit 0
- `sat` / `lanes-pin` / r4: before 35.22 35.39 35.47 5/3344 33444 (host CPU busy 16 %), with hog 35.45 35.44 35.49 23/3313 33598, governor performance , exit 0
- `sat` / `lanes-rt` / r1: before 36.63 25.27 17.26 28/3447 4109064 (host CPU busy 99 %), with hog 36.58 25.45 17.36 42/3110 4109307, governor performance , exit 0
- `sat` / `lanes-rt` / r2: before 30.65 35.82 30.08 15/3369 4161347 (host CPU busy 58 %), with hog 31.00 35.81 30.11 34/3384 4161690, governor performance , exit 0
- `sat` / `lanes-rt` / r3: before 32.63 34.99 35.36 8/3038 25523 (host CPU busy 23 %), with hog 32.63 34.99 35.36 25/3001 25680, governor performance , exit 0
- `sat` / `lanes-rt` / r4: before 32.01 33.63 34.75 9/3342 50129 (host CPU busy 12 %), with hog 30.57 33.30 34.64 28/3291 50287, governor performance , exit 0
- `sat` / `lanes-rt-nopin` / r1: before 44.19 36.82 24.26 33/3512 4126126 (host CPU busy 88 %), with hog 44.19 36.82 24.26 35/3402 4126471, governor performance , exit 0
- `sat` / `lanes-rt-nopin` / r2: before 38.71 38.59 32.64 17/3270 4177433 (host CPU busy 76 %), with hog 38.49 38.54 32.66 28/3285 4177528, governor performance , exit 0
- `sat` / `lanes-rt-nopin` / r3: before 39.44 38.26 36.04 18/3079 5505 (host CPU busy 58 %), with hog 39.44 38.26 36.04 32/3077 5705, governor performance , exit 0
- `sat` / `lanes-rt-nopin` / r4: before 31.86 33.00 34.25 5/3269 63147 (host CPU busy 13 %), with hog 31.86 33.00 34.25 27/3352 63351, governor performance , exit 0
- `sat` / `single` / r1: before 13.70 8.53 10.70 6/3348 4093297 (host CPU busy 13 %), with hog 13.70 8.53 10.70 22/3359 4093745, governor performance , exit 0
- `sat` / `single` / r2: before 36.87 38.96 35.68 13/3118 543 (host CPU busy 43 %), with hog 35.19 38.58 35.57 31/3141 622, governor performance , exit 0
- `sat` / `single` / r3: before 32.86 35.02 35.36 18/2901 19477 (host CPU busy 43 %), with hog 32.79 34.97 35.34 28/2940 19580, governor performance , exit 0
- `sat` / `single` / r4: before 33.33 35.19 35.42 5/3299 42830 (host CPU busy 18 %), with hog 31.79 34.84 35.31 23/3381 43051, governor performance , exit 0
