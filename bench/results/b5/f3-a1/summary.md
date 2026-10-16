# B-5 judge summary: f3-a1

```
label=f3-a1
utc=2026-10-16T18:08:30Z
git_rev=669c358 dirty=1
kernel=7.1.8+deb13-rt-amd64
cpus=16
governor=performance 
loadavg_start=14.46 21.17 34.55 2/3704 694817
rounds=5 arms=single,lanes-rt conds=sat only=A1 quick=no
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
  load: 14.46 21.17 34.55 2/3706 694847
loadavg_end=42.65 43.85 41.08 23/3714 761015
```

## Condition `sat`

Value = median over rounds [min..max]; `W` = rounds in which the arm was lower than `single` in the same round / rounds.

| series | pct | single | lanes-rt |
|---|---|---|---|
| A1 a1_ms | p50 | 7.809 [6.628..9.204] | 8.070 [6.959..9.824] W1/5 |
| A1 a1_ms | p95 | 16.244 [15.047..17.453] | 14.106 [12.541..18.532] W4/5 |
| A1 a1_ms | p99 | 29.582 [19.429..35.376] | 26.209 [24.911..32.056] W3/5 |

Lost (silent loss, max over rounds): A1: lanes-rt=1, single=4

## Reader CPU seconds per run (user+sys, all threads; same scenario set per run)

- `sat`: lanes-rt=1.1 [1.0..1.2], single=0.9 [0.9..1.0]

## Real-time grants

- `sat` / `lanes-rt`: 5 reader starts, all-lanes-RT in 5, grants {'rr:15': 5, 'rr:12': 10, 'rr:13': 5}, status ['rtkit available'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []
- `sat` / `single`: 5 reader starts, all-lanes-RT in 0, grants {}, status ['not requested'], watchdog demotions 0, SIGXCPU demotions 0, stray non-OTHER threads []

## Load per run

- `sat` / `lanes-rt` / r1: before 29.88 24.11 34.20 14/3400 701404 (host CPU busy 35 %), with hog 28.53 23.93 34.08 26/3527 701583, governor performance , exit 0
- `sat` / `lanes-rt` / r2: before 41.19 29.31 35.00 12/3511 707611 (host CPU busy 46 %), with hog 40.13 29.28 34.96 26/3523 708097, governor performance , exit 0
- `sat` / `lanes-rt` / r3: before 48.70 38.36 37.03 25/3622 728039 (host CPU busy 99 %), with hog 47.04 38.19 36.98 40/3449 728254, governor performance , exit 0
- `sat` / `lanes-rt` / r4: before 53.88 43.12 38.87 24/3308 735213 (host CPU busy 99 %), with hog 54.05 43.33 38.97 42/3372 735432, governor performance , exit 0
- `sat` / `lanes-rt` / r5: before 40.84 44.56 40.99 12/3672 752157 (host CPU busy 61 %), with hog 40.45 44.42 40.96 33/3680 752458, governor performance , exit 0
- `sat` / `single` / r1: before 14.46 21.17 34.55 6/3704 694849 (host CPU busy 15 %), with hog 13.94 20.95 34.41 24/3718 694899, governor performance , exit 0
- `sat` / `single` / r2: before 34.31 30.34 34.79 8/3511 715497 (host CPU busy 13 %), with hog 33.73 30.28 34.75 25/3532 715601, governor performance , exit 0
- `sat` / `single` / r3: before 45.65 34.64 35.83 25/3445 721482 (host CPU busy 100 %), with hog 45.65 34.64 35.83 41/3567 721744, governor performance , exit 0
- `sat` / `single` / r4: before 54.47 46.35 40.47 25/3515 739101 (host CPU busy 99 %), with hog 54.47 46.35 40.47 40/3540 739156, governor performance , exit 0
- `sat` / `single` / r5: before 49.40 47.16 41.35 5/3456 744930 (host CPU busy 14 %), with hog 46.32 46.56 41.19 24/3476 744990, governor performance , exit 0
