# B-5 judge summary: verdict-r1

```
label=verdict-r1 reviewer verdict run; binary code=d0a13c0 stamp=gorca e185a10
loadavg_start=50.57 52.70 46.67 17/3865 547129
rounds=3 arms=single,lanes-rt conds=idle,sat only=L3-10k,L3-50k,A1
               total        used        free      shared  buff/cache   available
Mem:              30          28           0           0           2           2
```

## Condition `idle`

Value = median over rounds [min..max]; `W` = rounds in which the arm was lower than `single` in the same round / rounds.

| series | pct | single | lanes-rt |
|---|---|---|---|
| L3-50k l1_ms | p50 | 5.264 [5.255..5.265] | 5.178 [5.175..5.180] W3/3 |
| L3-50k l1_ms | p95 | 5.650 [5.643..5.691] | 5.508 [5.496..5.512] W3/3 |
| L3-50k l1_ms | p99 | 6.333 [6.192..6.994] | 6.156 [5.979..6.440] W2/3 |
| A1 a1_ms | p50 | 4.073 [4.062..4.099] | 4.186 [4.084..4.309] W0/3 |
| A1 a1_ms | p95 | 22.489 [11.323..24.732] | 10.079 [6.702..11.580] W3/3 |
| A1 a1_ms | p99 | 75.885 [26.626..94.113] | 87.096 [25.184..131.328] W1/3 |

Lost (silent loss, max over rounds): L3-50k: lanes-rt=0, single=0; A1: lanes-rt=0, single=0

Superseded (info; sum over rounds): L3-50k: lanes-rt=19, single=12

Storm achieved rate (ev/s, median): L3-50k: lanes-rt=50000, single=50000

## Condition `sat`

Value = median over rounds [min..max]; `W` = rounds in which the arm was lower than `single` in the same round / rounds.

| series | pct | single | lanes-rt |
|---|---|---|---|
| L3-50k l1_ms | p50 | 9.657 [8.271..10.181] | 7.266 [7.058..7.475] W3/3 |
| L3-50k l1_ms | p95 | 21.979 [21.769..28.905] | 16.069 [15.418..17.336] W3/3 |
| L3-50k l1_ms | p99 | 36.399 [34.909..48.377] | 23.827 [21.421..33.859] W3/3 |
| A1 a1_ms | p50 | 9.537 [8.675..9.580] | 9.929 [9.242..9.994] W1/3 |
| A1 a1_ms | p95 | 22.045 [18.115..29.085] | 21.548 [13.586..23.569] W2/3 |
| A1 a1_ms | p99 | 83.554 [39.103..170.248] | 67.063 [36.971..97.222] W3/3 |

Lost (silent loss, max over rounds): L3-50k: lanes-rt=0, single=0; A1: lanes-rt=0, single=1

Superseded (info; sum over rounds): L3-50k: lanes-rt=392, single=720

Storm achieved rate (ev/s, median): L3-50k: lanes-rt=31776, single=33517

## Reader CPU seconds per run (user+sys, all threads; same scenario set per run)

- `idle`: lanes-rt=16.5 [16.3..16.8], single=15.6 [15.6..15.6]
- `sat`: lanes-rt=10.0 [4.6..10.1], single=6.7 [6.4..7.3]

## Real-time grants

- `idle` / `lanes-rt`: 9 reader starts, all-lanes-RT in 9, grants {'rr:15': 9, 'rr:12': 18, 'rr:13': 9}, status ['rtkit available'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []
- `idle` / `single`: 9 reader starts, all-lanes-RT in 0, grants {}, status ['not requested'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []
- `sat` / `lanes-rt`: 9 reader starts, all-lanes-RT in 9, grants {'rr:15': 9, 'rr:12': 18, 'rr:13': 9}, status ['rtkit available'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []
- `sat` / `single`: 9 reader starts, all-lanes-RT in 0, grants {}, status ['not requested'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []

## Load per run

- `idle` / `lanes-rt` / r1: before 50.00 51.66 47.10 8/3773 549039 (host CPU busy None %), with hog None, governor None, exit 0
- `idle` / `lanes-rt` / r2: before 62.34 58.88 51.87 9/3748 553992 (host CPU busy None %), with hog None, governor None, exit 0
- `idle` / `lanes-rt` / r3: before 52.58 58.95 55.76 4/3775 565058 (host CPU busy None %), with hog None, governor None, exit 0
- `idle` / `single` / r1: before 50.57 52.70 46.67 13/3866 547134 (host CPU busy None %), with hog None, governor None, exit 0
- `idle` / `single` / r2: before 43.68 53.04 50.67 8/3744 555517 (host CPU busy None %), with hog None, governor None, exit 0
- `idle` / `single` / r3: before 66.80 63.48 56.47 14/3775 561908 (host CPU busy None %), with hog None, governor None, exit 0
- `sat` / `lanes-rt` / r1: before 59.03 54.92 49.49 6/3780 552421 (host CPU busy None %), with hog None, governor None, exit 0
- `sat` / `lanes-rt` / r2: before 61.50 56.05 52.07 5/3769 557978 (host CPU busy None %), with hog None, governor None, exit 0
- `sat` / `lanes-rt` / r3: before 37.70 48.22 52.22 11/3118 580173 (host CPU busy None %), with hog None, governor None, exit 0
- `sat` / `single` / r1: before 45.07 49.54 46.96 9/3768 550838 (host CPU busy None %), with hog None, governor None, exit 0
- `sat` / `single` / r2: before 69.57 61.30 54.65 10/3775 560005 (host CPU busy None %), with hog None, governor None, exit 0
- `sat` / `single` / r3: before 43.08 53.32 54.15 8/3489 568360 (host CPU busy None %), with hog None, governor None, exit 0
