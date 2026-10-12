# B-2 candidate A: raw drain, lock-free ring, consumer-side coalescing

Branch `worktree-agent-a51937197a8c8515b`, code commit 0fc97fa. Three full `run-all.sh` runs
(`cand-a-r1..r3`, 3 repeats each, release build), reference baseline in
`bench/results/b2-ingress/reference/`. Host: 16 CPUs, RT kernel 7.1.8, **load average 13 to 18
during every run** (the reviewer must re-run on a quiet machine; load average at start and end of
each run is in the `meta` block of the JSON files):

| run | loadavg start | loadavg end |
|---|---|---|
| r1 | 18.51 | 17.68 |
| r2 | 15.09 | 16.66 |
| r3 | 18.01 | 17.13 |

## 1. Gate status (SPEC section 6)

| Gate | Result |
|---|---|
| `make check` (fmt, clippy -D warnings, test, deps) | green (debug) |
| conformance, debug and release (`cargo test -p gorca-bench-b2`) | all pass: ingress 6/6 incl. exact C-I1, drain 14/14, bounds 4/4, realbus 3/3 (the F3 fixture self-test failed once in a loaded debug run and passed on re-run: it does not touch candidate code) |
| B-5 giant message (`--include-ignored`) | **fails in the harness, not in the drain**: "RSS grew 201 MiB for one 100 MiB message". The test holds the 100 MiB string, and the mock broker `Link::flush` keeps the 100 MiB userspace queue capacity (`st.q.clear()` keeps capacity) until its pump thread frees it, after the fence. With a local patch to the mock (free the queue above 8 MiB) and the check moved after the string is dropped, RSS is 1 MiB and `malformed >= 1`: the drain skips the message while streaming (buffer stays 1 MiB). Needs a harness fix |
| C.failures@{all 7 mixes, 3 buses} | 0 in all three runs |
| T3.survived, T5.survived, T5.pending_max, T5.rss_delta | pass (pending_max@stall_lossless 65 536 of 69 632; rss 4 to 7 MiB) |
| FA.storm / mixed / hung failures | 0 / 0 / 0 in all three runs |
| T1.sustained_eps | r1 200k, r2 200k PASS; **r3 50k FAIL** (every step fails on `lag p99 20 ms`, the consumer-thread wake lag, with an empty backlog: p99 backlog 5 KiB; host load) |
| T4.drain_core_pct@100k | **FAIL** 65 / 71 / 73 % (limit 60, reference 94 %) |

Not passing the speed thresholds: T4 CPU per 10k events (136 to 147 ms, limit 25, reference 225)
and T4 drain core (above). T2.p99@100k and T7 pass. T3.focus_p99 sits on the 2 ms line
(2018 / 1962 / 2002 us; the reference is 1958): at 40k/s with 16 busy cores that is the harness
probe path, not the drain. Cause of T4: with a lightly loaded socket every message is one `recv`,
one `SIOCINQ` (skipped when the read did not fill the buffer) and one consumer wake (`Notify::notify_one`, a futex
or eventfd write on the drain thread, about 5 us). Per-message syscall cost, not parsing, dominates;
only batching the wake per `recv` (candidate C's idea) removes it.

## 2. Audit tests AU-1..AU-7 (SPEC 3.7), all un-ignored

| # | Test | Result |
|---|---|---|
| AU-1 | `audit_budget_released_when_ingress_dropped`, `audit_sender_churn_exhausts_global_budget_permanently` | pass. `Table::teardown` on receiver drop returns every charge; `Table::check` verifies `held == live + extra`; unit test `dropping_the_receiver_returns_the_budget` and `apps_of_one_factory_share_one_budget` |
| AU-2 | `audit_dead_receiver_does_not_hold_budget`; drain test `the_route_is_dropped_when_the_name_loses_its_owner` | pass. `push` into a closed ingress discards; the drain also subscribes to `NameOwnerChanged` and drops the cached route of a vanished unique name |
| AU-3 | `audit_hostile_apps_cannot_starve_a_victim` | pass. See design: preemption of the largest holder |
| AU-4 | `audit_text_deltas_are_charged_and_bounded` | pass. Extra deltas are charged 1 each and capped at `hard_cap` (a delta beyond it collapses the slot to `Replaced`) |
| AU-5 | `audit_drained_storm_releases_memory` | pass (16 drained apps, within the 16 MiB allowance) |
| AU-6 | `audit_single_push_is_bounded_under_tombstone_compaction`, `audit_drain_critical_section_is_bounded` | pass in debug and release. Measured: `drain()` of 65 536 events 1.7 us (lock held 1 us); compaction is a cursor walk of 64 positions per push |
| AU-7 | own test `concurrent_pushes_never_overshoot_the_global_budget` (6 apps, watcher thread, 25 consecutive clean runs) | pass. Charge is a CAS that never exceeds the limit; the newest focus preempts and retries before it may exceed by one, only when the limit is below the number of apps |

## 3. Comparison (compare.py: reference, r1, r2, r3)

`+` pass, `!` fail, per `compare.py`. Medians of 3 repeats per run.

```
metric                             bus              reference      cand-a-r1      cand-a-r2      cand-a-r3   threshold
T7.push_p50_ns                     -                      65  4  (0.06x better) 4  (0.06x better) 4  (0.06x better)   
T7.push_p99_ns                     -                     140+ 7+ (0.05x better) 7+ (0.05x better) 7+ (0.05x better)   <= 1,000
T7.push_p999_ns                    -                     657+ 15+ (0.02x better) 15+ (0.02x better) 16+ (0.02x better)   <= 5,000
T7.push_max_ns                     -                 356,417  15,587  (0.04x better) 24,228  (0.07x better) 15,473  (0.04x better)   
T7.push4_p50_ns                    -                     355  51  (0.14x better) 60  (0.17x better) 59  (0.17x better)   
T7.push4_p99_ns                    -                  21,392! 4,150+ (0.19x better) 4,837+ (0.23x better) 4,656+ (0.22x better)   <= 5,000
T7.push4_p999_ns                   -                  59,857! 36,638+ (0.61x better) 27,894+ (0.47x better) 27,140+ (0.45x better)   <= 50,000
T1.sustained_eps                   mock              200,000+       200,000+       200,000+ 50,000! (0.25x worse)   >= 100,000
T1.backlog_p99_kib                 mock               522.28  18.75  (0.04x better) 53.75  (0.10x better) 5  (0.01x better)   
T1.lag_p99_us                      mock               11,986  14,034  (1.17x worse) 15,008  (1.25x worse) 5,060  (0.42x better)   
T1.saturation_eps                  mock              306,473  5,277,800  (17.22x better) 4,889,778  (15.96x better) 5,396,466  (17.61x better)   
T2.p50_us@idle                     mock                61.08+ 36.91+ (0.60x better) 33.52+ (0.55x better) 30.27+ (0.50x better)   <= 300
T2.p95_us@idle                     mock               227.25  149.21  (0.66x better) 130.66  (0.57x better) 78.89  (0.35x better)   
T2.p99_us@idle                     mock               311.91+ 211.62+ (0.68x better) 192.78+ (0.62x better) 124.61+ (0.40x better)   <= 500
T2.p999_us@idle                    mock               588.62         575.17  545.68  (0.93x better) 530.61  (0.90x better)   
T2.max_us@idle                     mock                1,732  2,169  (1.25x worse) 1,640  (0.95x better) 1,305  (0.75x better)   
T2.p50_us@10k                      mock                47.15+ 31.99+ (0.68x better) 30.73+ (0.65x better) 29.30+ (0.62x better)   <= 300
T2.p95_us@10k                      mock               153.74         152.48  116.26  (0.76x better) 75.45  (0.49x better)   
T2.p99_us@10k                      mock               245.55+ 869.58+ (3.54x worse) 189.18+ (0.77x better) 147.39+ (0.60x better)   <= 1,000
T2.p999_us@10k                     mock               629.88+ 3,410! (5.41x worse) 556.89+ (0.88x better) 550.07+ (0.87x better)   <= 3,000
T2.max_us@10k                      mock                2,424  8,778  (3.62x worse)         2,430          2,373    
T2.p50_us@100k                     mock                38.80+ 29.61+ (0.76x better) 23.45+ (0.60x better) 21.41+ (0.55x better)   <= 300
T2.p95_us@100k                     mock                2,138  338.06  (0.16x better) 142.85  (0.07x better) 61.31  (0.03x better)   
T2.p99_us@100k                     mock                3,757! 971.77+ (0.26x better) 498.94+ (0.13x better) 149.79+ (0.04x better)   <= 3,000
T2.p999_us@100k                    mock                6,765+ 3,186+ (0.47x better) 2,351+ (0.35x better) 668.89+ (0.10x better)   <= 10,000
T2.max_us@100k                     mock                9,318  7,585  (0.81x better) 7,011  (0.75x better) 2,281  (0.24x better)   
T3.focus_p50_us                    mock                54.02+ 36.23+ (0.67x better) 32.54+ (0.60x better) 34.13+ (0.63x better)   <= 600
T3.focus_p99_us                    mock                1,958+         2,018!         1,962+         2,002!   <= 2,000
T3.focus_p999_us                   mock                2,161+         2,209+         2,105+         2,159+   <= 8,000
T3.focus_max_us                    mock                4,996  4,065  (0.81x better) 4,000  (0.80x better) 3,992  (0.80x better)   
T3.focus_unobserved                mock                    0+             0+             0+             0+   <= 0
T3.survived                        mock                    1+             1+             1+             1+   >= 1
T3.focus_p50_us                    dbus-daemon         96.24  85.31  (0.89x better) 73.57  (0.76x better)         94.21    
T3.focus_p99_us                    dbus-daemon         2,102          2,091  4,051  (1.93x worse) 3,347  (1.59x worse)   
T3.focus_p999_us                   dbus-daemon         3,103  4,731  (1.52x worse) 9,721  (3.13x worse) 5,551  (1.79x worse)   
T3.focus_max_us                    dbus-daemon         4,730  7,857  (1.66x worse) 13,812  (2.92x worse) 10,284  (2.17x worse)   
T3.focus_unobserved                dbus-daemon             0+             0+             0+             0+   <= 0
T3.survived                        dbus-daemon             1+             1+             1+             1+   >= 1
T3.focus_p50_us                    dbus-broker         91.02  966.18  (10.61x worse) 413.89  (4.55x worse) 85.76  (0.94x better)   
T3.focus_p99_us                    dbus-broker         2,064  21,232  (10.29x worse) 22,817  (11.06x worse) 3,193  (1.55x worse)   
T3.focus_p999_us                   dbus-broker         2,453  28,797  (11.74x worse) 33,304  (13.57x worse) 5,598  (2.28x worse)   
T3.focus_max_us                    dbus-broker         4,152  32,118  (7.73x worse) 35,805  (8.62x worse) 11,688  (2.81x worse)   
T3.focus_unobserved                dbus-broker             0+             0+             0+             0+   <= 0
T3.survived                        dbus-broker             1+             1+             1+             1+   >= 1
T4.idle_cpu_ms_per_s               mock                 0.02+ 0.01+ (0.49x better) 0.01+ (0.47x better) 0.01+ (0.42x better)   <= 10
T4.cpu_ms_per_10k@10k              mock               224.96! 147.04! (0.65x better) 139.94! (0.62x better) 135.97! (0.60x better)   <= 25
T4.cpu_ms_per_s@10k                mock               224.95  147.04  (0.65x better) 139.94  (0.62x better) 135.96  (0.60x better)   
T4.drain_core_pct@100k             mock                93.83! 65.12! (0.69x better) 70.78! (0.75x better) 72.88! (0.78x better)   <= 60
T4.cpu_ms_per_10k@100k             mock                93.83! 65.13! (0.69x better) 70.78! (0.75x better) 72.88! (0.78x better)   <= 25
T4.all_threads_core_pct@100k       mock                93.83  65.12  (0.69x better) 70.78  (0.75x better) 72.88  (0.78x better)   
T5.rss_delta_mib@distinct          mock                 6.33+ 8.75+ (1.38x worse) 8.96+ (1.42x worse) 8.82+ (1.39x worse)   <= 256
T5.pending_max@distinct            mock                  239+ 448+ (1.87x worse) 329+ (1.38x worse) 577+ (2.41x worse)   <= 69,632
T5.survived@distinct               mock                    1+             1+             1+             1+   >= 1
T5.flooded_batches@distinct        mock                    1              1              1              1    
C.violations@distinct              mock                    0+             0+             0+             0+   <= 0
T5.rss_delta_mib@stall             mock                 0.02+ 0+ (0.00x better) 0.23+ (12.00x worse) 0+ (0.00x better)   <= 256
T5.pending_max@stall               mock               30,035+        30,023+        30,036+        30,028+   <= 69,632
T5.survived@stall                  mock                    1+             1+             1+             1+   >= 1
T5.flooded_batches@stall           mock                    1              1              1              1    
C.violations@stall                 mock                    0+             0+             0+             0+   <= 0
T5.rss_delta_mib@stall_lossless    mock                20.92+ 6.98+ (0.33x better) 6.97+ (0.33x better) 4.13+ (0.20x better)   <= 256
T5.pending_max@stall_lossless      mock               65,536+        65,536+        65,536+        65,536+   <= 69,632
T5.survived@stall_lossless         mock                    1+             1+             1+             1+   >= 1
T5.flooded_batches@stall_lossless  mock                    1              1              1              1    
C.violations@stall_lossless        mock                    0+             0+             0+             0+   <= 0
C.failures@fakeapp                 mock                    0+             0+             0+             0+   <= 0
C.compared_nodes@fakeapp           mock                  799            799            799            799    
C.flooded_batches@fakeapp          mock                    0              0              0              0    
C.pushed_vs_sent@fakeapp           mock              282,739        282,741        282,740        282,740    
C.failures@terminal                mock                    0+             0+             0+             0+   <= 0
C.compared_nodes@terminal          mock                   12             12             12             12    
C.flooded_batches@terminal         mock                    0              0              0              0    
C.pushed_vs_sent@terminal          mock              251,496        251,498        251,497        251,496    
C.failures@realistic               mock                    0+             0+             0+             0+   <= 0
C.compared_nodes@realistic         mock                  830            830            830            830    
C.flooded_batches@realistic        mock                    0              0              0              0    
C.pushed_vs_sent@realistic         mock              261,382        261,383        261,383        261,382    
C.failures@lossless                mock                    0+             0+             0+             0+   <= 0
C.compared_nodes@lossless          mock                  650            650            649            649    
C.flooded_batches@lossless         mock                    0              0              0              0    
C.pushed_vs_sent@lossless          mock              269,114        269,113        269,112        269,112    
C.failures@realistic+flood         mock                    0+             0+             0+             0+   <= 0
C.compared_nodes@realistic+flood   mock                  148            148            148            148    
C.flooded_batches@realistic+flood  mock                   30             30             30             30    
C.pushed_vs_sent@realistic+flood   mock              261,382        261,384        261,384        261,383    
C.failures@distinct+flood          mock                    0+             0+             0+             0+   <= 0
C.compared_nodes@distinct+flood    mock                    9  6  (0.67x worse) 5  (0.56x worse) 4  (0.44x worse)   
C.flooded_batches@distinct+flood   mock                   30             30             30             30    
C.pushed_vs_sent@distinct+flood    mock              251,497        251,496        251,498        251,498    
C.failures@fakeapp                 dbus-daemon             0+             0+             0+             0+   <= 0
C.compared_nodes@fakeapp           dbus-daemon           770            770            770            770    
C.flooded_batches@fakeapp          dbus-daemon             0              0              0              0    
C.pushed_vs_sent@fakeapp           dbus-daemon       114,205        114,204        114,205        114,206    
C.failures@realistic               dbus-daemon             0+             0+             0+             0+   <= 0
C.compared_nodes@realistic         dbus-daemon           823            823            823            823    
C.flooded_batches@realistic        dbus-daemon             0              0              0              0    
C.pushed_vs_sent@realistic         dbus-daemon       105,468        105,469        105,467        105,469    
C.failures@fakeapp                 dbus-broker             0+             0+             0+             0+   <= 0
C.compared_nodes@fakeapp           dbus-broker           770            770            770            770    
C.flooded_batches@fakeapp          dbus-broker             0              0              0              0    
C.pushed_vs_sent@fakeapp           dbus-broker       114,206        114,203        114,205        114,204    
C.failures@realistic               dbus-broker             0+             0+             0+             0+   <= 0
C.compared_nodes@realistic         dbus-broker           823            823            823            823    
C.flooded_batches@realistic        dbus-broker             0              0              0              0    
C.pushed_vs_sent@realistic         dbus-broker       105,466        105,469        105,467        105,467    
F3.slow_reader_eof                 dbus-broker             1              1              1              1    
F3.active_reader_eof               dbus-broker             0              0              0              0    
F3.slow_reader_eof                 dbus-daemon             0              0              0              0    
F3.active_reader_eof               dbus-daemon             0              0              0              0    

reference: 42 pass, 6 fail  (git 1917d80, 2026-10-12T07:29:13Z, release, load 16.38 14.54 13.43 25/4692 332285)
cand-a-r1: 43 pass, 5 fail  (git 0fc97fa, 2026-10-12T09:12:49Z, release, load 18.51 14.34 13.84 7/3746 607421)
cand-a-r2: 45 pass, 3 fail  (git 0fc97fa, 2026-10-12T09:25:40Z, release, load 15.09 16.31 15.43 3/3794 634433)
cand-a-r3: 43 pass, 5 fail  (git 0fc97fa, 2026-10-12T09:37:53Z, release, load 18.01 17.27 16.39 7/4067 665065)
```

Highlights against the reference: T7 push p99 140 -> 7 ns, push4 p99 21.4 -> 4.2 to 4.8 us
(now passes), push4 p99.9 59.9 -> 27 to 37 us (passes); T1.saturation 306k -> 4.9 to 5.4 M messages/s
(17x); T2.p99@100k 3757 -> 150 to 970 us (passes); T4 CPU per 10k at 10k/s 225 -> 136 to 147 ms and drain
core 94 -> 65 to 73 % (both still over the SPEC limits). The T3 real-broker rows are noisy on this loaded host
(dbus-broker p99 3.2 to 22.8 ms across runs) and carry no threshold.

## 4. Design summary (what runs where, what is bounded by what)

* **Drain** (`crates/gorca-dbus/src/drain/{raw,wire}.rs`, no zbus): one `gorca-drain-0` thread,
  blocking `read` into a 1 MiB buffer (grows to one message up to 16 MiB, shrinks back; a larger
  message is skipped while streaming and counted `malformed`), own framing from the 16-byte header,
  header-field walk (sender, path, interface, member, signature), the SPEC 3.2 mapping through the
  unchanged `map_signal`, `SIOCINQ` only when a read filled the buffer. SASL EXTERNAL, `Hello`, one
  pipelined `AddMatch` per rule plus the `NameOwnerChanged` rule, all under `connect_deadline`.
  Route cache: one-entry fast path, then a hash map of at most 4096 senders. `stop()` shuts the
  socket down; panics are caught and reported; counters are flushed once per read.
* **Ingress** (`crates/gorca-ingress/src/{coalesce,table,hash}.rs`): `push` is one CAS on a
  1024-record `crossbeam_queue::ArrayQueue` plus a wake flag (`Notify` only on the first push after
  a drain). Records are folded **in ring order** into a table that keeps the pending events in output
  order in one `Vec<NodeEvent>`, so a single producer sees exactly the executable model (C-I1 passes
  unmodified) wherever the fold happens. The fold runs in `drain()`, or in a producer when the ring
  is full (128-record chunk) or when the app already holds at least a ring's worth of slots. `drain()`
  swaps the vector out (O(1), no copy) and frees large buffers when the batch is dropped.
  Superseded slots are placeholders removed by an incremental compaction cursor (64 steps per push).
  Per-node index of 9 positions per node, seeded multiplicative hash.
* **Bounds**: ring 1024 records; table `max_keys` 4096, `hard_cap` 65 536 (slots, and separately
  queued text deltas); dirty set 65 536; global `SlotBudget` 262 144 with preemption (below);
  announcements 16/drain, text 4096 bytes; sender cache 4096; message 16 MiB; idle drained
  ingress keeps at most 4096 slots of capacity.
* **Global budget**: `SlotBudget::try_charge` is a CAS below the limit. An app holding fewer than
  64 slots that finds the pool empty takes the slots of the largest holder back (`shed`: that app
  becomes flooded and dirty-all, its newest focus survives, its announcements are counted dropped).
  Budget wiring: `gorca_dbus::drain::IngressFactory` creates every app ingress through one shared
  budget; routers must use it. **The harness `HarnessRouter` still calls bare `ingress()`**, so
  the benchmarks run without a budget; the budget paths are covered by the audit, bounds and unit tests.
* **Deviation from the brief**: the per-key generation table and lazy skipping of stale records
  were not built. Exact first-occurrence order needs every record applied, so lazy skipping saves
  nothing; the laziness is in *where* the fold happens.

## 5. Known weaknesses

1. T4: per-message syscalls (recv, futex wake) cost 14 us of CPU per message at 10k/s; over the SPEC limit.
2. Wake lag under load fails T1.sustained in some runs (r3); a delayed or batched wake per `recv` would help.
3. Preemption lets a new app force a resync (dirty-all) on the largest holder; a hostile process opening
   many connections can make a legitimate large app resync repeatedly (no data loss, bounded cost, but not free).
4. The focus charge can exceed the limit by one per app when the limit is below the number of apps.
5. Compaction/flood-entry: flood entry costs O(nodes) once per drain (about 100 us at 4096 nodes).
6. Code size: about 2 300 added non-test lines (coalesce 470, table 880, hash 85, raw 630, wire 220, factory 40), over the 1 600 budget by roughly 40 %; the audit requirements (preemption, charging, teardown) and own framing account for most of it. Zero new dependencies but `crossbeam-queue`.
7. `AddMatch` for `NameOwnerChanged` is an extra rule beyond `DrainConfig::match_rules`.

## 6. Raw `b2 bench` output

### cand-a-r1
```
metric                     bus                median          min          max  n op    threshold  res   note
T7.push_p50_ns             -                       4            4            4  3                  info   [ns]
T7.push_p99_ns             -                       7            6            7  3 <=         1000  PASS   [ns]
T7.push_p999_ns            -                      15           15           15  3 <=         5000  PASS   [ns]
T7.push_max_ns             -                   15587         6147        30071  3                  info   [ns]
T7.push4_p50_ns            -                      51           50           58  3                  info   [ns (4 producers)]
T7.push4_p99_ns            -                    4150         2967         4204  3 <=         5000  PASS   [ns (4 producers)]
T7.push4_p999_ns           -                   36638        28973        48150  3 <=        50000  PASS   [ns (4 producers)]
T1.sustained_eps           mock               200000       200000       400000  3 >=       100000  PASS  lag p99 20.0 ms at 400000/s [events/s]
T1.backlog_p99_kib         mock                 18.8        6.250         36.2  3                  info   [KiB at the sustained rate]
T1.lag_p99_us              mock                14034        10021        15045  3                  info   [us at the sustained rate]
T1.saturation_eps          mock              5277800      5005051      5420864  3                  info   [messages/s]
T2.p50_us@idle             mock                 36.9         32.8         38.8  3 <=          300  PASS  n=39999 [us]
T2.p95_us@idle             mock                149.2        133.2        169.1  3                  info   [us]
T2.p99_us@idle             mock                211.6        199.5        245.3  3 <=          500  PASS   [us]
T2.p999_us@idle            mock                575.2        549.3        674.5  3                  info   [us]
T2.max_us@idle             mock                 2169         2083         4200  3                  info   [us]
T2.p50_us@10k              mock                 32.0         27.8         32.8  3 <=          300  PASS  n=39999 [us]
T2.p95_us@10k              mock                152.5         76.0        188.9  3                  info   [us]
T2.p99_us@10k              mock                869.6        130.3         1226  3 <=         1000  PASS   [us]
T2.p999_us@10k             mock                 3410        540.2         4219  3 <=         3000  FAIL   [us]
T2.max_us@10k              mock                 8778         1397         9647  3                  info   [us]
T2.p50_us@100k             mock                 29.6         29.4         34.9  3 <=          300  PASS  n=39999 [us]
T2.p95_us@100k             mock                338.1        251.3        464.4  3                  info   [us]
T2.p99_us@100k             mock                971.8        784.4         1273  3 <=         3000  PASS   [us]
T2.p999_us@100k            mock                 3186         2638         3875  3 <=        10000  PASS   [us]
T2.max_us@100k             mock                 7585         5746         9562  3                  info   [us]
T3.focus_p50_us            mock                 36.2         35.7         38.2  3 <=          600  PASS   [us]
T3.focus_p99_us            mock                 2018         1994         2024  3 <=         2000  FAIL   [us]
T3.focus_p999_us           mock                 2209         2147         2211  3 <=         8000  PASS   [us]
T3.focus_max_us            mock                 4065         4041         4112  3                  info   [us]
T3.focus_unobserved        mock                    0            0            0  3 <=            0  PASS   [probes]
T3.survived                mock                    1            1            1  3 >=            1  PASS   [1=yes]
T3.focus_p50_us            dbus-daemon          85.3         75.3        107.1  3                  info   [us]
T3.focus_p99_us            dbus-daemon          2091         2004        14039  3                  info   [us]
T3.focus_p999_us           dbus-daemon          4731         4643        19595  3                  info   [us]
T3.focus_max_us            dbus-daemon          7857         6031        23443  3                  info   [us]
T3.focus_unobserved        dbus-daemon             0            0            0  3 <=            0  PASS   [probes]
T3.survived                dbus-daemon             1            1            1  3 >=            1  PASS   [1=yes]
T3.focus_p50_us            dbus-broker         966.2        808.9         1631  3                  info   [us]
T3.focus_p99_us            dbus-broker         21232        18238        22005  3                  info   [us]
T3.focus_p999_us           dbus-broker         28797        24479        39184  3                  info   [us]
T3.focus_max_us            dbus-broker         32118        27990        45300  3                  info   [us]
T3.focus_unobserved        dbus-broker             0            0            0  3 <=            0  PASS   [probes]
T3.survived                dbus-broker             1            1            1  3 >=            1  PASS   [1=yes]
T4.idle_cpu_ms_per_s       mock                0.009        0.007        0.010  3 <=           10  PASS   [ms/s]
T4.cpu_ms_per_10k@10k      mock                147.0        146.5        148.9  3 <=           25  FAIL   [ms per 10k events]
T4.cpu_ms_per_s@10k        mock                147.0        146.5        148.9  3                  info   [ms/s]
T4.drain_core_pct@100k     mock                 65.1         63.3         68.1  3 <=           60  FAIL   [% of one core]
T4.cpu_ms_per_10k@100k     mock                 65.1         63.3         68.1  3 <=           25  FAIL   [ms per 10k events]
T4.all_threads_core_pct@100k mock                 65.1         63.3         68.1  3                  info   [% of one core]
T5.rss_delta_mib@distinct  mock                8.754        8.746         24.1  3 <=          256  PASS   [MiB]
T5.pending_max@distinct    mock                  448          299          572  3 <=        69632  PASS   [events in one batch]
T5.survived@distinct       mock                    1            1            1  3 >=            1  PASS   [1=yes]
T5.flooded_batches@distinct mock                    1            1            1  3                  info   [batches]
C.violations@distinct      mock                    0            0            0  3 <=            0  PASS   [count]
T5.rss_delta_mib@stall     mock                    0            0            0  3 <=          256  PASS   [MiB]
T5.pending_max@stall       mock                30023        30002        30033  3 <=        69632  PASS   [events in one batch]
T5.survived@stall          mock                    1            1            1  3 >=            1  PASS   [1=yes]
T5.flooded_batches@stall   mock                    1            1            1  3                  info   [batches]
C.violations@stall         mock                    0            0            0  3 <=            0  PASS   [count]
T5.rss_delta_mib@stall_lossless mock                6.977        4.148         27.6  3 <=          256  PASS   [MiB]
T5.pending_max@stall_lossless mock                65536        65536        65536  3 <=        69632  PASS   [events in one batch]
T5.survived@stall_lossless mock                    1            1            1  3 >=            1  PASS   [1=yes]
T5.flooded_batches@stall_lossless mock                    1            1            1  3                  info   [batches]
C.violations@stall_lossless mock                    0            0            0  3 <=            0  PASS   [count]
C.failures@fakeapp         mock                    0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@fakeapp   mock                  799          799          799  1                  info   [nodes]
C.flooded_batches@fakeapp  mock                    0            0            0  1                  info   [batches]
C.pushed_vs_sent@fakeapp   mock               282741       282741       282741  1                  info   [pushes]
C.failures@terminal        mock                    0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@terminal  mock                   12           12           12  1                  info   [nodes]
C.flooded_batches@terminal mock                    0            0            0  1                  info   [batches]
C.pushed_vs_sent@terminal  mock               251498       251498       251498  1                  info   [pushes]
C.failures@realistic       mock                    0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@realistic mock                  830          830          830  1                  info   [nodes]
C.flooded_batches@realistic mock                    0            0            0  1                  info   [batches]
C.pushed_vs_sent@realistic mock               261383       261383       261383  1                  info   [pushes]
C.failures@lossless        mock                    0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@lossless  mock                  650          650          650  1                  info   [nodes]
C.flooded_batches@lossless mock                    0            0            0  1                  info   [batches]
C.pushed_vs_sent@lossless  mock               269113       269113       269113  1                  info   [pushes]
C.failures@realistic+flood mock                    0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@realistic+flood mock                  148          148          148  1                  info   [nodes]
C.flooded_batches@realistic+flood mock                   30           30           30  1                  info   [batches]
C.pushed_vs_sent@realistic+flood mock               261384       261384       261384  1                  info   [pushes]
C.failures@distinct+flood  mock                    0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@distinct+flood mock                    6            6            6  1                  info   [nodes]
C.flooded_batches@distinct+flood mock                   30           30           30  1                  info   [batches]
C.pushed_vs_sent@distinct+flood mock               251496       251496       251496  1                  info   [pushes]
C.failures@fakeapp         dbus-daemon             0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@fakeapp   dbus-daemon           770          770          770  1                  info   [nodes]
C.flooded_batches@fakeapp  dbus-daemon             0            0            0  1                  info   [batches]
C.pushed_vs_sent@fakeapp   dbus-daemon        114204       114204       114204  1                  info   [pushes]
C.failures@realistic       dbus-daemon             0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@realistic dbus-daemon           823          823          823  1                  info   [nodes]
C.flooded_batches@realistic dbus-daemon             0            0            0  1                  info   [batches]
C.pushed_vs_sent@realistic dbus-daemon        105469       105469       105469  1                  info   [pushes]
C.failures@fakeapp         dbus-broker             0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@fakeapp   dbus-broker           770          770          770  1                  info   [nodes]
C.flooded_batches@fakeapp  dbus-broker             0            0            0  1                  info   [batches]
C.pushed_vs_sent@fakeapp   dbus-broker        114203       114203       114203  1                  info   [pushes]
C.failures@realistic       dbus-broker             0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@realistic dbus-broker           823          823          823  1                  info   [nodes]
C.flooded_batches@realistic dbus-broker             0            0            0  1                  info   [batches]
C.pushed_vs_sent@realistic dbus-broker        105469       105469       105469  1                  info   [pushes]
F3.slow_reader_eof         dbus-broker             1            1            1  1                  info  sent 1.2 MiB, reader got 1.1 MiB (missing 0.0 MiB), sender_failed=true Broken pipe (os error 32) [1=disconnected]
F3.active_reader_eof       dbus-broker             0            0            0  1                  info  sent 96.0 MiB, reader got 102.9 MiB (missing 0.0 MiB), sender_failed=false  [1=disconnected]
F3.slow_reader_eof         dbus-daemon             0            0            0  1                  info  sent 96.0 MiB, reader got 8.6 MiB (missing 87.4 MiB), sender_failed=false  [1=disconnected]
F3.active_reader_eof       dbus-daemon             0            0            0  1                  info  sent 96.0 MiB, reader got 102.9 MiB (missing 0.0 MiB), sender_failed=false  [1=disconnected]
```

### cand-a-r2
```
metric                     bus                median          min          max  n op    threshold  res   note
T7.push_p50_ns             -                       4            4            4  3                  info   [ns]
T7.push_p99_ns             -                       7            6            7  3 <=         1000  PASS   [ns]
T7.push_p999_ns            -                      15           15           15  3 <=         5000  PASS   [ns]
T7.push_max_ns             -                   24228        13273        49152  3                  info   [ns]
T7.push4_p50_ns            -                      60           54           62  3                  info   [ns (4 producers)]
T7.push4_p99_ns            -                    4837         4229         4880  3 <=         5000  PASS   [ns (4 producers)]
T7.push4_p999_ns           -                   27894        26176        33186  3 <=        50000  PASS   [ns (4 producers)]
T1.sustained_eps           mock               200000       200000       200000  3 >=       100000  PASS  lag p99 24.9 ms at 400000/s [events/s]
T1.backlog_p99_kib         mock                 53.8         33.5         57.5  3                  info   [KiB at the sustained rate]
T1.lag_p99_us              mock                15008        15007        15018  3                  info   [us at the sustained rate]
T1.saturation_eps          mock              4889778      4721375      5095841  3                  info   [messages/s]
T2.p50_us@idle             mock                 33.5         32.8         36.3  3 <=          300  PASS  n=39999 [us]
T2.p95_us@idle             mock                130.7        118.4        188.7  3                  info   [us]
T2.p99_us@idle             mock                192.8        191.2        266.2  3 <=          500  PASS   [us]
T2.p999_us@idle            mock                545.7        532.4        564.0  3                  info   [us]
T2.max_us@idle             mock                 1640        823.0         2138  3                  info   [us]
T2.p50_us@10k              mock                 30.7         27.8         33.7  3 <=          300  PASS  n=39999 [us]
T2.p95_us@10k              mock                116.3         81.3        145.2  3                  info   [us]
T2.p99_us@10k              mock                189.2        157.7        240.7  3 <=         1000  PASS   [us]
T2.p999_us@10k             mock                556.9        549.3         2065  3 <=         3000  PASS   [us]
T2.max_us@10k              mock                 2430         1840         6198  3                  info   [us]
T2.p50_us@100k             mock                 23.5         23.1         25.8  3 <=          300  PASS  n=39999 [us]
T2.p95_us@100k             mock                142.8         69.9        238.5  3                  info   [us]
T2.p99_us@100k             mock                498.9        197.8         1595  3 <=         3000  PASS   [us]
T2.p999_us@100k            mock                 2351        772.7         8912  3 <=        10000  PASS   [us]
T2.max_us@100k             mock                 7011         3329        23711  3                  info   [us]
T3.focus_p50_us            mock                 32.5         30.4         34.0  3 <=          600  PASS   [us]
T3.focus_p99_us            mock                 1962        412.2         1973  3 <=         2000  PASS   [us]
T3.focus_p999_us           mock                 2105         2050         2113  3 <=         8000  PASS   [us]
T3.focus_max_us            mock                 4000         2349         4032  3                  info   [us]
T3.focus_unobserved        mock                    0            0            0  3 <=            0  PASS   [probes]
T3.survived                mock                    1            1            1  3 >=            1  PASS   [1=yes]
T3.focus_p50_us            dbus-daemon          73.6         73.5         77.7  3                  info   [us]
T3.focus_p99_us            dbus-daemon          4051         2184        16963  3                  info   [us]
T3.focus_p999_us           dbus-daemon          9721         4543        27488  3                  info   [us]
T3.focus_max_us            dbus-daemon         13812         5259        45060  3                  info   [us]
T3.focus_unobserved        dbus-daemon             0            0            3  3 <=            0  PASS   [probes]
T3.survived                dbus-daemon             1            1            1  3 >=            1  PASS   [1=yes]
T3.focus_p50_us            dbus-broker         413.9         64.8        545.1  3                  info   [us]
T3.focus_p99_us            dbus-broker         22817         3470       726325  3                  info   [us]
T3.focus_p999_us           dbus-broker         33304         8480       760872  3                  info   [us]
T3.focus_max_us            dbus-broker         35805        14717       770859  3                  info   [us]
T3.focus_unobserved        dbus-broker             0            0            0  3 <=            0  PASS   [probes]
T3.survived                dbus-broker             1            1            1  3 >=            1  PASS   [1=yes]
T4.idle_cpu_ms_per_s       mock                0.009        0.005        0.012  3 <=           10  PASS   [ms/s]
T4.cpu_ms_per_10k@10k      mock                139.9        138.8        140.1  3 <=           25  FAIL   [ms per 10k events]
T4.cpu_ms_per_s@10k        mock                139.9        138.8        140.1  3                  info   [ms/s]
T4.drain_core_pct@100k     mock                 70.8         69.3         73.8  3 <=           60  FAIL   [% of one core]
T4.cpu_ms_per_10k@100k     mock                 70.8         69.3         73.8  3 <=           25  FAIL   [ms per 10k events]
T4.all_threads_core_pct@100k mock                 70.8         69.3         73.8  3                  info   [% of one core]
T5.rss_delta_mib@distinct  mock                8.961        8.906         27.9  3 <=          256  PASS   [MiB]
T5.pending_max@distinct    mock                  329          322          611  3 <=        69632  PASS   [events in one batch]
T5.survived@distinct       mock                    1            1            1  3 >=            1  PASS   [1=yes]
T5.flooded_batches@distinct mock                    1            1            1  3                  info   [batches]
C.violations@distinct      mock                    0            0            0  3 <=            0  PASS   [count]
T5.rss_delta_mib@stall     mock                0.234        0.164        1.984  3 <=          256  PASS   [MiB]
T5.pending_max@stall       mock                30036        30033        30036  3 <=        69632  PASS   [events in one batch]
T5.survived@stall          mock                    1            1            1  3 >=            1  PASS   [1=yes]
T5.flooded_batches@stall   mock                    1            1            1  3                  info   [batches]
C.violations@stall         mock                    0            0            0  3 <=            0  PASS   [count]
T5.rss_delta_mib@stall_lossless mock                6.973        4.148         20.4  3 <=          256  PASS   [MiB]
T5.pending_max@stall_lossless mock                65536        65536        65536  3 <=        69632  PASS   [events in one batch]
T5.survived@stall_lossless mock                    1            1            1  3 >=            1  PASS   [1=yes]
T5.flooded_batches@stall_lossless mock                    1            1            1  3                  info   [batches]
C.violations@stall_lossless mock                    0            0            0  3 <=            0  PASS   [count]
C.failures@fakeapp         mock                    0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@fakeapp   mock                  799          799          799  1                  info   [nodes]
C.flooded_batches@fakeapp  mock                    0            0            0  1                  info   [batches]
C.pushed_vs_sent@fakeapp   mock               282740       282740       282740  1                  info   [pushes]
C.failures@terminal        mock                    0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@terminal  mock                   12           12           12  1                  info   [nodes]
C.flooded_batches@terminal mock                    0            0            0  1                  info   [batches]
C.pushed_vs_sent@terminal  mock               251497       251497       251497  1                  info   [pushes]
C.failures@realistic       mock                    0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@realistic mock                  830          830          830  1                  info   [nodes]
C.flooded_batches@realistic mock                    0            0            0  1                  info   [batches]
C.pushed_vs_sent@realistic mock               261383       261383       261383  1                  info   [pushes]
C.failures@lossless        mock                    0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@lossless  mock                  649          649          649  1                  info   [nodes]
C.flooded_batches@lossless mock                    0            0            0  1                  info   [batches]
C.pushed_vs_sent@lossless  mock               269112       269112       269112  1                  info   [pushes]
C.failures@realistic+flood mock                    0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@realistic+flood mock                  148          148          148  1                  info   [nodes]
C.flooded_batches@realistic+flood mock                   30           30           30  1                  info   [batches]
C.pushed_vs_sent@realistic+flood mock               261384       261384       261384  1                  info   [pushes]
C.failures@distinct+flood  mock                    0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@distinct+flood mock                    5            5            5  1                  info   [nodes]
C.flooded_batches@distinct+flood mock                   30           30           30  1                  info   [batches]
C.pushed_vs_sent@distinct+flood mock               251498       251498       251498  1                  info   [pushes]
C.failures@fakeapp         dbus-daemon             0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@fakeapp   dbus-daemon           770          770          770  1                  info   [nodes]
C.flooded_batches@fakeapp  dbus-daemon             0            0            0  1                  info   [batches]
C.pushed_vs_sent@fakeapp   dbus-daemon        114205       114205       114205  1                  info   [pushes]
C.failures@realistic       dbus-daemon             0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@realistic dbus-daemon           823          823          823  1                  info   [nodes]
C.flooded_batches@realistic dbus-daemon             0            0            0  1                  info   [batches]
C.pushed_vs_sent@realistic dbus-daemon        105467       105467       105467  1                  info   [pushes]
C.failures@fakeapp         dbus-broker             0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@fakeapp   dbus-broker           770          770          770  1                  info   [nodes]
C.flooded_batches@fakeapp  dbus-broker             0            0            0  1                  info   [batches]
C.pushed_vs_sent@fakeapp   dbus-broker        114205       114205       114205  1                  info   [pushes]
C.failures@realistic       dbus-broker             0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@realistic dbus-broker           823          823          823  1                  info   [nodes]
C.flooded_batches@realistic dbus-broker             0            0            0  1                  info   [batches]
C.pushed_vs_sent@realistic dbus-broker        105467       105467       105467  1                  info   [pushes]
F3.slow_reader_eof         dbus-broker             1            1            1  1                  info  sent 1.1 MiB, reader got 1.1 MiB (missing 0.0 MiB), sender_failed=true Broken pipe (os error 32) [1=disconnected]
F3.active_reader_eof       dbus-broker             0            0            0  1                  info  sent 96.0 MiB, reader got 102.9 MiB (missing 0.0 MiB), sender_failed=false  [1=disconnected]
F3.slow_reader_eof         dbus-daemon             0            0            0  1                  info  sent 96.0 MiB, reader got 8.6 MiB (missing 87.4 MiB), sender_failed=false  [1=disconnected]
F3.active_reader_eof       dbus-daemon             0            0            0  1                  info  sent 96.0 MiB, reader got 102.9 MiB (missing 0.0 MiB), sender_failed=false  [1=disconnected]
```

### cand-a-r3
```
metric                     bus                median          min          max  n op    threshold  res   note
T7.push_p50_ns             -                       4            3            4  3                  info   [ns]
T7.push_p99_ns             -                       7            6           13  3 <=         1000  PASS   [ns]
T7.push_p999_ns            -                      16           15           20  3 <=         5000  PASS   [ns]
T7.push_max_ns             -                   15473        10089        24130  3                  info   [ns]
T7.push4_p50_ns            -                      59           56           61  3                  info   [ns (4 producers)]
T7.push4_p99_ns            -                    4656         4352         4833  3 <=         5000  PASS   [ns (4 producers)]
T7.push4_p999_ns           -                   27140        27124        33872  3 <=        50000  PASS   [ns (4 producers)]
T1.sustained_eps           mock                50000            0       100000  3 >=       100000  FAIL  lag p99 21.4 ms at 200000/s [events/s]
T1.backlog_p99_kib         mock                    5            0         12.5  3                  info   [KiB at the sustained rate]
T1.lag_p99_us              mock                 5060            0        10018  3                  info   [us at the sustained rate]
T1.saturation_eps          mock              5396466      5106012      5621012  3                  info   [messages/s]
T2.p50_us@idle             mock                 30.3         30.0         31.8  3 <=          300  PASS  n=39999 [us]
T2.p95_us@idle             mock                 78.9         74.3         81.1  3                  info   [us]
T2.p99_us@idle             mock                124.6        119.3        132.2  3 <=          500  PASS   [us]
T2.p999_us@idle            mock                530.6        526.9        533.1  3                  info   [us]
T2.max_us@idle             mock                 1305         1057         1310  3                  info   [us]
T2.p50_us@10k              mock                 29.3         28.4         31.0  3 <=          300  PASS  n=39999 [us]
T2.p95_us@10k              mock                 75.5         60.1         92.4  3                  info   [us]
T2.p99_us@10k              mock                147.4         90.3        169.3  3 <=         1000  PASS   [us]
T2.p999_us@10k             mock                550.1        540.2        564.6  3 <=         3000  PASS   [us]
T2.max_us@10k              mock                 2373         1189         2772  3                  info   [us]
T2.p50_us@100k             mock                 21.4         20.3         22.2  3 <=          300  PASS  n=39999 [us]
T2.p95_us@100k             mock                 61.3         47.3         62.1  3                  info   [us]
T2.p99_us@100k             mock                149.8        110.0        154.7  3 <=         3000  PASS   [us]
T2.p999_us@100k            mock                668.9        636.2        922.1  3 <=        10000  PASS   [us]
T2.max_us@100k             mock                 2281         1917         2709  3                  info   [us]
T3.focus_p50_us            mock                 34.1         33.6         39.3  3 <=          600  PASS   [us]
T3.focus_p99_us            mock                 2002         1954         2027  3 <=         2000  FAIL   [us]
T3.focus_p999_us           mock                 2159         2117         2308  3 <=         8000  PASS   [us]
T3.focus_max_us            mock                 3992         3946         4078  3                  info   [us]
T3.focus_unobserved        mock                    0            0            0  3 <=            0  PASS   [probes]
T3.survived                mock                    1            1            1  3 >=            1  PASS   [1=yes]
T3.focus_p50_us            dbus-daemon          94.2         78.1         94.6  3                  info   [us]
T3.focus_p99_us            dbus-daemon          3347         2757         3440  3                  info   [us]
T3.focus_p999_us           dbus-daemon          5551         5394         8692  3                  info   [us]
T3.focus_max_us            dbus-daemon         10284         7929        15887  3                  info   [us]
T3.focus_unobserved        dbus-daemon             0            0            0  3 <=            0  PASS   [probes]
T3.survived                dbus-daemon             1            1            1  3 >=            1  PASS   [1=yes]
T3.focus_p50_us            dbus-broker          85.8         80.4        121.0  3                  info   [us]
T3.focus_p99_us            dbus-broker          3193         2798         7001  3                  info   [us]
T3.focus_p999_us           dbus-broker          5598         4860        11330  3                  info   [us]
T3.focus_max_us            dbus-broker         11688         9902        15196  3                  info   [us]
T3.focus_unobserved        dbus-broker             0            0            0  3 <=            0  PASS   [probes]
T3.survived                dbus-broker             1            1            1  3 >=            1  PASS   [1=yes]
T4.idle_cpu_ms_per_s       mock                0.008        0.006        0.011  3 <=           10  PASS   [ms/s]
T4.cpu_ms_per_10k@10k      mock                136.0        136.0        139.5  3 <=           25  FAIL   [ms per 10k events]
T4.cpu_ms_per_s@10k        mock                136.0        136.0        139.5  3                  info   [ms/s]
T4.drain_core_pct@100k     mock                 72.9         72.7         73.1  3 <=           60  FAIL   [% of one core]
T4.cpu_ms_per_10k@100k     mock                 72.9         72.7         73.1  3 <=           25  FAIL   [ms per 10k events]
T4.all_threads_core_pct@100k mock                 72.9         72.7         73.1  3                  info   [% of one core]
T5.rss_delta_mib@distinct  mock                8.824        8.770         23.7  3 <=          256  PASS   [MiB]
T5.pending_max@distinct    mock                  577          317          708  3 <=        69632  PASS   [events in one batch]
T5.survived@distinct       mock                    1            1            1  3 >=            1  PASS   [1=yes]
T5.flooded_batches@distinct mock                    1            1            1  3                  info   [batches]
C.violations@distinct      mock                    0            0            0  3 <=            0  PASS   [count]
T5.rss_delta_mib@stall     mock                    0            0         11.7  3 <=          256  PASS   [MiB]
T5.pending_max@stall       mock                30028        30025        30031  3 <=        69632  PASS   [events in one batch]
T5.survived@stall          mock                    1            1            1  3 >=            1  PASS   [1=yes]
T5.flooded_batches@stall   mock                    1            1            1  3                  info   [batches]
C.violations@stall         mock                    0            0            0  3 <=            0  PASS   [count]
T5.rss_delta_mib@stall_lossless mock                4.129        4.125         16.2  3 <=          256  PASS   [MiB]
T5.pending_max@stall_lossless mock                65536        65536        65536  3 <=        69632  PASS   [events in one batch]
T5.survived@stall_lossless mock                    1            1            1  3 >=            1  PASS   [1=yes]
T5.flooded_batches@stall_lossless mock                    1            1            1  3                  info   [batches]
C.violations@stall_lossless mock                    0            0            0  3 <=            0  PASS   [count]
C.failures@fakeapp         mock                    0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@fakeapp   mock                  799          799          799  1                  info   [nodes]
C.flooded_batches@fakeapp  mock                    0            0            0  1                  info   [batches]
C.pushed_vs_sent@fakeapp   mock               282740       282740       282740  1                  info   [pushes]
C.failures@terminal        mock                    0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@terminal  mock                   12           12           12  1                  info   [nodes]
C.flooded_batches@terminal mock                    0            0            0  1                  info   [batches]
C.pushed_vs_sent@terminal  mock               251496       251496       251496  1                  info   [pushes]
C.failures@realistic       mock                    0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@realistic mock                  830          830          830  1                  info   [nodes]
C.flooded_batches@realistic mock                    0            0            0  1                  info   [batches]
C.pushed_vs_sent@realistic mock               261382       261382       261382  1                  info   [pushes]
C.failures@lossless        mock                    0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@lossless  mock                  649          649          649  1                  info   [nodes]
C.flooded_batches@lossless mock                    0            0            0  1                  info   [batches]
C.pushed_vs_sent@lossless  mock               269112       269112       269112  1                  info   [pushes]
C.failures@realistic+flood mock                    0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@realistic+flood mock                  148          148          148  1                  info   [nodes]
C.flooded_batches@realistic+flood mock                   30           30           30  1                  info   [batches]
C.pushed_vs_sent@realistic+flood mock               261383       261383       261383  1                  info   [pushes]
C.failures@distinct+flood  mock                    0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@distinct+flood mock                    4            4            4  1                  info   [nodes]
C.flooded_batches@distinct+flood mock                   30           30           30  1                  info   [batches]
C.pushed_vs_sent@distinct+flood mock               251498       251498       251498  1                  info   [pushes]
C.failures@fakeapp         dbus-daemon             0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@fakeapp   dbus-daemon           770          770          770  1                  info   [nodes]
C.flooded_batches@fakeapp  dbus-daemon             0            0            0  1                  info   [batches]
C.pushed_vs_sent@fakeapp   dbus-daemon        114206       114206       114206  1                  info   [pushes]
C.failures@realistic       dbus-daemon             0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@realistic dbus-daemon           823          823          823  1                  info   [nodes]
C.flooded_batches@realistic dbus-daemon             0            0            0  1                  info   [batches]
C.pushed_vs_sent@realistic dbus-daemon        105469       105469       105469  1                  info   [pushes]
C.failures@fakeapp         dbus-broker             0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@fakeapp   dbus-broker           770          770          770  1                  info   [nodes]
C.flooded_batches@fakeapp  dbus-broker             0            0            0  1                  info   [batches]
C.pushed_vs_sent@fakeapp   dbus-broker        114204       114204       114204  1                  info   [pushes]
C.failures@realistic       dbus-broker             0            0            0  1 <=            0  PASS  order=0 missed_wakes=0 misrouted=0 unfenced=0 vacuous=0 [count]
C.compared_nodes@realistic dbus-broker           823          823          823  1                  info   [nodes]
C.flooded_batches@realistic dbus-broker             0            0            0  1                  info   [batches]
C.pushed_vs_sent@realistic dbus-broker        105467       105467       105467  1                  info   [pushes]
F3.slow_reader_eof         dbus-broker             1            1            1  1                  info  sent 1.2 MiB, reader got 1.1 MiB (missing 0.0 MiB), sender_failed=true Broken pipe (os error 32) [1=disconnected]
F3.active_reader_eof       dbus-broker             0            0            0  1                  info  sent 96.0 MiB, reader got 102.9 MiB (missing 0.0 MiB), sender_failed=false  [1=disconnected]
F3.slow_reader_eof         dbus-daemon             0            0            0  1                  info  sent 96.0 MiB, reader got 8.6 MiB (missing 87.4 MiB), sender_failed=false  [1=disconnected]
F3.active_reader_eof       dbus-daemon             0            0            0  1                  info  sent 96.0 MiB, reader got 102.9 MiB (missing 0.0 MiB), sender_failed=false  [1=disconnected]
```
