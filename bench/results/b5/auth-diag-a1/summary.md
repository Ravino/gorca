# B-5 judge summary: auth-diag-a1

```
label=auth-diag-a1
utc=2026-10-16T12:04:50Z
git_rev=e185a10 dirty=5
kernel=7.1.8+deb13-rt-amd64
cpus=16
governor=performance 
loadavg_start=26.60 28.14 28.58 6/3278 4063180
rounds=5 arms=single,lanes,lanes-rt,lanes-rt-nopin,lanes-pin conds=sat only=A1,K3 quick=no
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
  load: 26.60 28.14 28.58 4/3280 4063209
loadavg_end=22.05 25.08 27.35 4/2864 4067989
```

## Reader CPU seconds per run (user+sys, all threads; same scenario set per run)


## Real-time grants


## Load per run

- `sat` / `lanes` / r1: before 26.31 28.06 28.55 4/3275 4063388 (host CPU busy 8 %), with hog 25.80 27.92 28.51 19/3283 4063439, governor performance , exit 1
- `sat` / `lanes` / r2: before 24.89 27.61 28.40 7/3266 4064124 (host CPU busy 9 %), with hog 24.42 27.47 28.35 19/3282 4064171, governor performance , exit 1
- `sat` / `lanes` / r3: before 15.98 24.84 27.42 3/3258 4065774 (host CPU busy 7 %), with hog 15.98 24.84 27.42 28/3291 4065822, governor performance , exit 1
- `sat` / `lanes` / r4: before 18.81 24.90 27.38 3/2868 4066743 (host CPU busy 7 %), with hog 18.81 24.90 27.38 18/2877 4066773, governor performance , exit 1
- `sat` / `lanes` / r5: before 22.05 25.24 27.44 6/2874 4067460 (host CPU busy 9 %), with hog 22.05 25.24 27.44 18/2888 4067515, governor performance , exit 1
- `sat` / `lanes-pin` / r1: before 25.06 27.69 28.43 4/3275 4063930 (host CPU busy 9 %), with hog 24.89 27.61 28.40 20/3282 4063973, governor performance , exit 1
- `sat` / `lanes-pin` / r2: before 23.67 27.22 28.26 5/3259 4064658 (host CPU busy 9 %), with hog 22.17 26.85 28.13 19/3290 4064709, governor performance , exit 1
- `sat` / `lanes-pin` / r3: before 17.63 25.62 27.71 6/3278 4065410 (host CPU busy 7 %), with hog 16.38 25.23 27.57 19/3292 4065446, governor performance , exit 1
- `sat` / `lanes-pin` / r4: before 16.90 24.75 27.36 5/3268 4066134 (host CPU busy 7 %), with hog 16.90 24.75 27.36 18/3007 4066192, governor performance , exit 1
- `sat` / `lanes-pin` / r5: before 20.15 24.99 27.38 6/2866 4067106 (host CPU busy 8 %), with hog 20.15 24.99 27.38 21/2890 4067154, governor performance , exit 1
- `sat` / `lanes-rt` / r1: before 25.80 27.92 28.51 6/3267 4063575 (host CPU busy 9 %), with hog 25.50 27.82 28.47 18/3289 4063627, governor performance , exit 1
- `sat` / `lanes-rt` / r2: before 24.42 27.47 28.35 7/3265 4064302 (host CPU busy 8 %), with hog 25.03 27.55 28.37 22/3285 4064334, governor performance , exit 1
- `sat` / `lanes-rt` / r3: before 20.47 26.42 27.98 10/3274 4065020 (host CPU busy 9 %), with hog 19.08 26.03 27.85 21/3283 4065067, governor performance , exit 1
- `sat` / `lanes-rt` / r4: before 19.47 24.94 27.38 3/2858 4066909 (host CPU busy 9 %), with hog 19.47 24.94 27.38 20/2875 4066964, governor performance , exit 1
- `sat` / `lanes-rt` / r5: before 21.89 25.15 27.40 3/2873 4067646 (host CPU busy 8 %), with hog 21.89 25.15 27.40 18/2888 4067685, governor performance , exit 1
- `sat` / `lanes-rt-nopin` / r1: before 25.50 27.82 28.47 9/3280 4063758 (host CPU busy 9 %), with hog 25.06 27.69 28.43 18/3292 4063803, governor performance , exit 1
- `sat` / `lanes-rt-nopin` / r2: before 25.03 27.55 28.37 4/3259 4064463 (host CPU busy 17 %), with hog 23.67 27.22 28.26 19/3276 4064506, governor performance , exit 1
- `sat` / `lanes-rt-nopin` / r3: before 19.08 26.03 27.85 6/3276 4065215 (host CPU busy 8 %), with hog 17.63 25.62 27.71 21/3291 4065252, governor performance , exit 1
- `sat` / `lanes-rt-nopin` / r4: before 16.46 24.79 27.39 6/3274 4065960 (host CPU busy 9 %), with hog 16.46 24.79 27.39 20/3289 4065994, governor performance , exit 1
- `sat` / `lanes-rt-nopin` / r5: before 22.14 25.15 27.38 4/2870 4067820 (host CPU busy 9 %), with hog 22.14 25.15 27.38 20/2879 4067853, governor performance , exit 1
- `sat` / `single` / r1: before 26.60 28.14 28.58 3/3278 4063211 (host CPU busy 8 %), with hog 26.31 28.06 28.55 18/3292 4063257, governor performance , exit 1
- `sat` / `single` / r2: before 22.17 26.85 28.13 3/3277 4064851 (host CPU busy 7 %), with hog 20.47 26.42 27.98 18/3295 4064893, governor performance , exit 1
- `sat` / `single` / r3: before 16.38 25.23 27.57 4/3268 4065584 (host CPU busy 9 %), with hog 15.63 24.92 27.46 22/3278 4065628, governor performance , exit 1
- `sat` / `single` / r4: before 18.27 24.90 27.39 7/2972 4066405 (host CPU busy 7 %), with hog 18.27 24.90 27.39 19/2884 4066604, governor performance , exit 1
- `sat` / `single` / r5: before 21.10 25.11 27.41 4/2874 4067283 (host CPU busy 7 %), with hog 21.10 25.11 27.41 18/2896 4067322, governor performance , exit 1
