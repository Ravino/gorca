# P-12: real-time scheduling for gorca hot threads (host: Debian 13, 7.1.8+deb13-rt-amd64)

Research only. Nothing on the system was changed. All "verified" items were read from the live host
on 2026-10-13. Items marked **(upstream)** come from rtkit/systemd/kernel documented behaviour and
were not exercised here.

## 1. Verdict

On this host gorca **cannot get SCHED_FIFO today** by any route without an admin action:

| Route | State on host | Verified by |
|---|---|---|
| plain `sched_setscheduler` as user | **EPERM**: RLIMIT_RTPRIO=0, CapEff=0 | `chrt -f 1 true` -> `Operation not permitted`; `/proc/self/limits` |
| rtkit | **not installed**, no `org.freedesktop.RealtimeKit1` on system bus, "not activatable" | `apt-cache policy rtkit` -> Installed: (none), candidate 0.13-5.1; `busctl get-property` error |
| `@pipewire` group rlimit file | file exists (`/etc/security/limits.d/25-pw-rlimits.conf`: rtprio 95, nice -19, memlock 4M) but group `pipewire` (gid 108) has **no members**; user is not in it | `getent group pipewire` |
| `audio` group rtprio | user is in `audio`, but no `@audio rtprio` rule exists (Debian's `realtime-privileges`/`rtprio` file is absent) | `/etc/security/limits.conf` + `limits.d/*` |
| systemd user unit `CPUSchedulingPolicy=fifo` | would fail `214/SETSCHEDULER` (EPERM): user manager has CapEff=0 and rtprio 0 (inferred from kernel rule, unit not started to avoid side effects) | `systemctl --user show -p DefaultLimitRTPRIO` -> 0 |
| `CAP_SYS_NICE` / `AmbientCapabilities` in user unit | not grantable: user manager cannot grant capabilities it does not hold | CapPrm 0 |

Consequence seen live: PipeWire on this host is **also not real-time**. Journal:
`mod.rt: RTKit error: org.freedesktop.DBus.Error.ServiceUnknown`, `RTKit does not give us
MaxRealtimePriority, using 1`, `... MinNiceLevel, using 0`. Its `data-loop.0` thread is `TS` (SCHED_OTHER)
and its nice is 0 (`chrt -p`, `ps -L`). So the 21 ms PipeWire quantum measured earlier already runs
non-RT; gorca-side RT would not change end-to-end audio start.

Existing FIFO threads on host are kernel only (`idle_inject/*`, `card1-crtc*`, all prio 50).

## 2. rtkit

- Interface: system bus, `org.freedesktop.RealtimeKit1`, path `/org/freedesktop/RealtimeKit1`.
- Properties (all read-only, signature): `MaxRealtimePriority` i, `MinNiceLevel` i, `RTTimeUSecMax` x.
  Method signatures (confirmed from `refs/mutter/src/tests/dbusmock-templates/rtkit.py` and mutter's
  client `refs/mutter/src/backends/native/meta-thread.c:255-360`):
  - `MakeThreadRealtime(t thread, u priority)`
  - `MakeThreadRealtimeWithPID(t pid, t thread, u priority)`
  - `MakeThreadHighPriority(t thread, i priority)` (nice)
  - `MakeThreadHighPriorityWithPID(t pid, t thread, i priority)`
  - `ResetKnown()`, `ResetAll()` are privileged.
- Values **on this host: unknown, daemon not installed.** Debian 0.13 defaults **(upstream)**:
  MaxRealtimePriority 20, MinNiceLevel -15, RTTimeUSecMax 200000 us. The mutter dbusmock mirrors those
  numbers. Re-read the three properties after the user installs rtkit (command in §8).
- Requirements **(upstream)**:
  1. Caller's `RLIMIT_RTTIME` must be set (not infinity) and <= `RTTimeUSecMax`, else the request is refused. Set
     soft = hard = RTTimeUSecMax **before** calling, in the requesting process (mutter does exactly this,
     `meta-thread.c:347-349`).
  2. Requested priority <= MaxRealtimePriority. Thread id is the kernel TID (`gettid`), not pthread_t.
  3. polkit action `org.freedesktop.RealtimeKit1.acquire-real-time` (and `acquire-high-priority`),
     default allow for active local sessions. A systemd user service in the user session is covered by
     the caller's uid check on the system bus; not verified here.
  4. Daemon-side budgets: a per-user cap on RT threads and a rate limit on grants (burst 5 per
     10 s approx.), and a **canary watchdog**: a higher-priority canary thread; if it is starved the
     daemon demotes every thread it promoted.
  5. rtkit sets SCHED_RESET_ON_FORK itself; children do not inherit RT.
- PipeWire usage (`libpipewire-module-rt`, shipped in `/usr/share/pipewire/pipewire.conf`: `nice.level
  = -11`, `rt.prio = 88`, `rt.time.soft/hard = -1` = default): first try `sched_setscheduler` with the
  rlimit RTPRIO granted by `@pipewire` limits.d; on EPERM falls back to rtkit: read `MaxRealtimePriority`
  and `MinNiceLevel`, clamp its wanted priority to them, set RLIMIT_RTTIME to `RTTimeUSecMax`, call
  `MakeThreadRealtime`, and on failure `MakeThreadHighPriority` (nice). Each data-loop thread is
  promoted individually via a helper thread `module-rt` (visible in `ps -L`). gorca should copy this
  order: rlimit path first, rtkit second, nice third, plain third-class last.

## 3. systemd user units and limits

Verified: systemd 257.13; `systemctl --user show` -> `DefaultLimitRTPRIO=0`, `DefaultLimitRTTIME=infinity`;
`/etc/systemd/user.conf` has only commented defaults. `/etc/systemd/system/user@.service.d/override.conf`
only adds `SupplementaryGroups=proc`. `/usr/lib/systemd/user.conf.d/` holds only the coredump drop-in.

- The user manager inherits its limits from PAM (`systemd-user` service, not verified here) and its own RLIMIT_RTPRIO is 0; a unit can never exceed the manager's hard limit
  but the hard limit it can hand out is capped by the manager's own `RLIMIT_RTPRIO`. Both are 0 here.
- Fix paths (admin only, none applied):
  - (A) install `rtkit` (+ the Debian `realtime-privileges` package optionally): gives rtkit route.
  - (B) `/etc/security/limits.d/` entry `@audio - rtprio 20` (or add the user to `pipewire`), then
    re-login; the user manager then has hard rtprio 20 and units can set `LimitRTPRIO=20`,
    `CPUSchedulingPolicy=fifo`, `CPUSchedulingPriority=10`, `LimitRTTIME=200ms`.
  - (C) system-level drop-in `user@.service.d/` with `LimitRTPRIO=` and `AmbientCapabilities=`: wider than
    needed; not recommended.
- Unit keys worth having once a path exists: `LimitRTTIME=200ms` (bounds a runaway, see §5),
  `CPUSchedulingResetOnFork=yes`, `Nice=-5`. Per-thread priorities cannot be set by a unit
  (it sets the main thread; threads inherit unless reset); gorca must set per-thread policy itself.
- `RestrictRealtime=` is **not** set in `contrib/systemd/gorca.service` (comment confirms). Good; setting it
  would block all of the above.
- No `RT_GROUP_SCHED`: cgroup v2 (`cgroup2` mounted, no `cpu.rt_runtime_us`), so no per-cgroup RT bandwidth.
  User manager has only controllers `cpu memory pids` delegated (no `cpuset`), so `AllowedCPUs=` in a user
  unit does not work; use `sched_setaffinity` in-process (always allowed for own threads).

## 4. CPU topology, pinning, isolation, governor

Verified (`lscpu -e`, `/sys/devices/system/cpu`):
- Intel Core Ultra 9 285H, 16 CPUs, 1 thread per core (no SMT), one NUMA node.
- Hybrid: CPU 0-5 P-cores (max 5400 MHz), CPU 6-13 E-cores (4500 MHz), CPU 14-15 LP-E cores (2500 MHz,
  separate cache domain, slow). Pin hot threads to P-cores, **never** to 14-15.
- Kernel cmdline has **no** `isolcpus`, `nohz_full`, `rcu_nocbs`, `irqaffinity`; `/sys/devices/system/cpu/isolated`
  and `nohz_full` are empty. Nothing is isolated, so pinning only expresses preference; other tasks and
  IRQs can still land on the chosen core. Cmdline is also `lockdown=confidentiality`, `init_on_alloc=1`,
  `slab_debug=FZ` (hardening costs, relevant for benchmark noise, not for scheduling).
- Changing cmdline needs root + reboot; for a desktop screen reader, isolating cores is overkill. Optional
  later: `isolcpus=managed_irq,domain,2-3 irqaffinity=0-1,4-13` is **not** recommended by default.
- cpufreq: driver `intel_pstate` (active mode), governor `powersave` on cpu0 (available: performance,
  powersave), EPP `balance_performance`, min 400 MHz, max 5400 MHz, `no_turbo`=0. `scaling_governor`
  is `-rw-r--r-- root`: **a user cannot change it** (needs root or power-profiles-daemon; the latter is
  not installed, `powerprofilesctl` absent). Under intel_pstate active+powersave, "powersave" is the
  HWP-driven dynamic policy, not a fixed low clock; EPP is the real lever. For authoritative B-5 runs the
  user must set `performance` (or EPP `performance`) as root; plan already says so (phase-2 §P0 risks).
- Scheduler tunables (verified): `kernel.sched_rt_runtime_us=950000`, `sched_rt_period_us=1000000`,
  `sched_rr_timeslice_ms=100`, `kernel.watchdog=1`. `sched_autogroup_enabled` absent.
  Kernel is PREEMPT_RT (name `-rt`; `/boot/config` unreadable for user, not double-checked).
  Threaded IRQs on RT run at FIFO 50; gorca FIFO prio must stay **below 50** to avoid delaying
  input/GPU IRQ threads, and below rtkit's own max anyway.

## 5. Safety: bounding a runaway SCHED_FIFO thread

Layers, innermost first:
1. **RLIMIT_RTTIME** (per-thread cumulative CPU in RT without a blocking syscall; reset by any sleep/block).
   Exceeding the soft limit sends SIGXCPU, hard limit SIGKILL. Set to 100-200 ms. Handle SIGXCPU in a
   dedicated signal thread: log, then demote the offending thread to SCHED_OTHER via
   `sched_setscheduler(tid, SCHED_OTHER)` (allowed: lowering needs no privilege) rather than die.
   Caveat: SIGXCPU is process-directed; the handler needs to know which thread (keep a per-lane heartbeat
   counter and demote any lane whose heartbeat stalls). rtkit **requires** this limit anyway.
2. **Design rule:** an RT lane only does bounded work per wake-up: pop N items, then block in `epoll`/
   futex. No spin loops, no unbounded `try_recv` loops, no allocation-heavy parsing on the RT thread
   (parse on SCHED_OTHER workers; RT thread forwards fds/bytes). Max batch is a compile-time constant
   with `rt_budget_us` check via `clock_gettime(CLOCK_THREAD_CPUTIME_ID)`; yield after budget.
3. **Userspace watchdog:** a SCHED_OTHER (nice 0) thread checks per-lane heartbeats every 50 ms; if a lane
   misses 4, it demotes it and sets `rt_degraded` metric/log. Works even though the RT thread is stuck,
   as long as another core is free (it is: 16 cores) and RT throttling is on.
4. **Kernel RT throttling:** `sched_rt_runtime_us=950000/1000000` guarantees SCHED_OTHER tasks 5% of every
   CPU per second, so a full-time FIFO thread cannot fully lock a core, only starve it to 5%. Do not
   touch it (disabling = -1 is the dangerous setting). On this host the system is at the default.
5. **rtkit canary** (if used): the daemon demotes all granted threads if its canary starves. Free watchdog.
6. **systemd:** `LimitRTTIME=200ms` mirrors layer 1 for the main thread; `Restart=on-failure` already in
   the unit. `kernel.watchdog=1` catches hard lockups of an entire CPU in last resort (soft lockup
   detector fires only for non-RT-preempted cases; RT throttling makes it unlikely).
7. **Never** FIFO the tokio runtime workers: any async task that spins would pin a core at RT prio. Only
   the dedicated hot-lane threads (ingress drain, speech lane, key input) are candidates.

Priority policy: use FIFO 10-20 (<= rtkit's 20, well below IRQ threads at 50). Input/key lane = 15,
ingress drain = 12, speech lane = 12; same-priority peers on different cores rarely compete. Use
`SCHED_FIFO | SCHED_RESET_ON_FORK` so subprocesses (speechd launch, helper tools) do not inherit RT.

## 6. Rust APIs and minimum code

Crates already in the offline registry (`.toolchain/cargo/registry`): `libc 0.2.190`, `rustix 1.1.5`, `zbus 5.19`.
`thread-priority`, `nix`, `core_affinity` are **not** vendored (adding needs the user to allow network;
not required). Recommendation: **`libc` directly** plus `zbus` (already a dependency), ~80 lines in a
`gorca-rt` helper module, no new crate.

| API | Use | Note |
|---|---|---|
| `libc::sched_setscheduler(tid, policy, &param)` | policy per thread (Linux accepts TID) | `SCHED_FIFO \| SCHED_RESET_ON_FORK` |
| `libc::pthread_setschedparam(pthread_self(), ...)` | same, via pthread id | portable wrapper; uses TID under the hood |
| `libc::setrlimit(RLIMIT_RTTIME, ..)` | required for rtkit and for safety | per-process, set before first RT request |
| `libc::sched_setaffinity(0, ...)`/`CPU_SET` | pin thread | works unprivileged on own threads |
| `libc::gettid` via `syscall(SYS_gettid)` | tid for rtkit | pthread_t is not a TID |
| `libc::setpriority(PRIO_PROCESS, tid, nice)` | nice fallback; negative needs RLIMIT_NICE or rtkit | |
| `thread-priority` crate | thin wrapper over the above; not vendored, rtkit support needs its optional D-Bus feature | skip |
| rtkit over zbus | `MakeThreadRealtime(tid, prio)` on **system** bus | use blocking-in-a-helper-thread or async call with deadline; never from the RT thread itself |

Minimum flow (called once per hot thread, from that thread, at start; failure is non-fatal):

```rust
// 1. try rlimit route
let p = libc::sched_param { sched_priority: prio };
let r = unsafe { libc::sched_setscheduler(0, libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK, &p) };
if r == 0 { return Mode::Fifo(prio) }                           // errno EPERM -> go on
// 2. rtkit route
let rl = libc::rlimit { rlim_cur: rttime_us, rlim_max: rttime_us };
unsafe { libc::setrlimit(libc::RLIMIT_RTTIME, &rl) };
let tid = unsafe { libc::syscall(libc::SYS_gettid) } as u64;
// zbus (system bus, own short-lived connection, 250 ms method timeout):
// proxy("org.freedesktop.RealtimeKit1", "/org/freedesktop/RealtimeKit1", "org.freedesktop.RealtimeKit1")
// props: MaxRealtimePriority:i32, MinNiceLevel:i32, RTTimeUSecMax:i64 ; clamp prio, rttime
// call MakeThreadRealtime(tid: u64, prio: u32)  -> on error MakeThreadHighPriority(tid, min_nice: i32)
// 3. else stay SCHED_OTHER; log once; export rt_mode metric (Off | Nice | Fifo(prio))
```

Notes: do the rtkit D-Bus calls on a normal worker with deadline, passing the target TID
(`MakeThreadRealtimeWithPID(pid, tid, prio)` if the request is made by a different thread; plain
`MakeThreadRealtime` also takes any TID of the caller's process). The RLIMIT_RTTIME must be in place before
the call; the limit is process-wide so set it once at startup. Pinning: `sched_setaffinity(0, mask of P-cores 0-5)`
from the thread (determine P-cores by `cpu*/cpufreq/cpuinfo_max_freq` ranking or
`/sys/devices/cpu_core/cpus` on hybrid Intel; ignore if absent).

## 7. Plan for B-5 arm (c): (b) + rtkit SCHED_FIFO + pinning

Make arm (c) a **runtime mode of arm (b)**, selected by config `rt.mode = off | nice | fifo` and
`rt.pin = off | p-cores`, so the same binary does the A/B.

1. **`gorca-rt::sched` module** (rust-engineer): the fallback chain of §6, per-thread, returns the achieved
   `Mode`; logs the exact errno; exports achieved mode into the bench report (a run whose requested mode
   was not achieved is marked INVALID, not silently "off").
2. **Lanes eligible:** input (keys), ingress drain, speech lane writer. Priorities 15 / 12 / 12 (below IRQ
   threads at 50). Everything else (tokio workers, rayon, WASM, D-Bus call connections) stays SCHED_OTHER.
3. **Safety wiring before first enable:** RLIMIT_RTTIME 200 ms, SIGXCPU handler -> demote, 50 ms heartbeat
   watchdog, bounded batches (§5). Add a test lane that busy-loops and prove it gets demoted and the
   host stays interactive (test-engineer, runs in a private PID/user namespace? no: just under `LimitRTTIME`).
4. **Pinning:** hot lanes on distinct P-cores (e.g. input 4, ingress 5, speech 3), tokio/rayon workers get the
   mask `0-2,6-13` so that they do not share the pinned cores; never use 14-15. No isolcpus: report this
   as a known limitation of the A/B.
5. **Host prerequisites (user must run, one-time; I did not):**
   - `sudo apt-get install --no-install-recommends rtkit` (Debian trixie 0.13-5.1). Starts via system-bus
     activation; `systemctl status rtkit-daemon` should show it afterwards. This **also fixes PipeWire's
     realtime** on this host (a side benefit that can shift end-to-end audio latency; note it in
     the benchmark baseline: run B-5 with and without rtkit active for PipeWire, or treat as constant).
   - Or instead of rtkit: `/etc/security/limits.d/26-gorca.conf` with `@audio - rtprio 20` and `- nice -10`
     (user already in `audio`; needs re-login). Simpler, no daemon, no canary.
   - For authoritative runs: governor/EPP `performance` as root; quiet host.
   - Check after install: `busctl --system get-property org.freedesktop.RealtimeKit1
     /org/freedesktop/RealtimeKit1 org.freedesktop.RealtimeKit1 MaxRealtimePriority` (and `MinNiceLevel`,
     `RTTimeUSecMax`).
6. **Metric:** L1/L2/K1 p99 under L3-50k + saturated cores for arms (b) vs (c); plus scheduling-latency
   histogram of the lane thread (`perf sched` or `/proc/<tid>/schedstat` run_delay deltas) so the benefit
   is visible even if end-to-end noise hides it.
7. **Unit file:** keep `RestrictRealtime` unset; add `LimitRTTIME=200ms` and `CPUSchedulingResetOnFork=yes` as
   commented lines until a privilege path exists (do not set `CPUSchedulingPolicy=fifo` in the unit: it
   fails the whole service with 214/SETSCHEDULER when the path is absent).
8. **Expectation (low confidence):** on RT kernel with 16 cores, gains from FIFO show only at the tail
   (p99.9) under saturation; pinning to P-cores may give more than FIFO because E-cores are ~17% slower
   and LP-E cores are far slower. If B-5 shows < 10% p99 gain, ship `rt.mode=nice`-or-off by default and keep
   FIFO opt-in.

## 8. Open items

- rtkit properties on a live daemon unread; Debian defaults taken from upstream/mutter mock.
- polkit rule behaviour for a systemd user service not verified.
- Was not tested: unit with `CPUSchedulingPolicy=fifo` (inference from EPERM rule; avoids creating units).
- `/boot/config-*` unreadable, so PREEMPT_RT/RT_GROUP_SCHED built-ins are inferred (cgroup2 files absent).
- Hybrid-core scheduling via Intel Thread Director (ITD) may migrate unpinned threads to E-cores
  under load, a possible hidden confounder for A/B arms (a)/(b); pinning arm (c) removes it, so
  consider also a pinned-only arm.
