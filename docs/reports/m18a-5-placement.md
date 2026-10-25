# M18a-5: hybrid-CPU placement and the idle-state lease

Date 2026-10-25/26, rust-engineer. Raw runs, scripts and tables: `bench/results/m18a5/`
(`res-*.txt` raw, `summary-*.txt` / `stages-lease.txt` made by `tab.py` / `stage.py`). Host: Core Ultra 9 285H
(P 0-5, E 6-13, LP-E 14-15, no L3 on LP-E), governor `powersave`, `intel_idle`. Harness: `plugin_latency`
(M18a-3/4), sps, ten first-party dummy plugins, 400 trials, 24 ms and 200 ms key spacing, variants
interleaved, three repeats. `load1` is in the summary files (1.2-11); the host was not always below 4, runs
with a load spike are kept and labelled by their `load1` column, none deleted.

## 1. What shipped

* `gorca_rt::topology` (small, no daemon types): detection from `/sys/devices/cpu_core|cpu_atom/cpus`
  (an atom core without L3 while P cores have one is LP-E), fallback `cpu_capacity`, then
  `cpuinfo_max_freq`, else no split. P cores are reduced to the biggest L3 group. One info line
  (`cpu placement: ...`) states what was found or why nothing is pinned. `TurnPin` (soft, restores the old mask).
* `gorca_plugin_host::actor::WorkerPlacement` / `ActorRunner::new_placed`: one affinity **mask per plugin**, set by the
  worker thread itself at start (replacements of a wedged actor included). Never a single-core pin.
* Daemon: waited-for plugins (not `refinement`, not untrusted/dev tier) get the P cluster, **capped at
  P-1 workers**, the rest P+E (never LP-E); refinement and third-party get the E cores. The writer thread is left alone.
* `[plugins] placement = "auto" | "off"` (default auto), applies only with `scheduler = "sps"`.
* `gorca_rt::pmqos::PmQosLease` + `[plugins] idle_lease = "auto" | "off"` (default auto), owner decision DP-1 (b):
  a 20 us `/dev/cpu_dma_latency` request, taken by a key-down that arms workers, released 2 s after the last
  one, cut and retaken after 30 s, no-op with one `pm-qos-unavailable` line if the node is not writable.
  Udev rule `packaging/udev/70-gorca-cpu-dma-latency.rules` + `packaging/udev/README.md`; **not installed**.
* Docs: `docs/config-reference.md`, ADR-0026 section 2.4.7.10 + changelog, `docs/decisions.md`.

## 2. Findings that shaped the placement (what did NOT work)

| shape | result (sps unless noted) | file |
|---|---|---|
| v1: all waited workers + writer on the 6 P cores (the literal task) | **worse**: labels facts 0.94 -> 0.10 (10 spinners in 6 cores), sps no gain, p99 up | `res-v1-strict-all-p.txt` |
| v2/v3/v4 variants (P+E without LP-E; cap P-1; writer moved) | labels worse in every shape (0.79 -> 0.18-0.48 at 200 ms); sps neutral | `res-v2-variants.txt` |
| v3 vs off, quiet host, writer moved off LP-E | guest call -25 %, but turn p99 at 200 ms 331 -> 872-2614 us: moving a running thread (affinity syscall) is a millisecond tail on ~1 turn in 100 | `res-v3-writer-wide.txt`, `res-v5-final-shape.txt` |
| **v5 (shipped): cap P-1, rest P+E, writer untouched, sps only** | guest call p50 90 -> 70 us (24 ms keys), -10 % at 200 ms; turn p50 unchanged; p99 not worse | `res-v5-final-shape.txt` |

So: a P mask is only safe when it does not oversubscribe the 6 P cores, and the writer must not be moved.
Under `labels` (wakes every worker every key) placement is switched off (one log line).

## 3. Before / after, gate C-4 (sps, first-party, `res-lease.txt`, medians of 3)

"placement" = shipped v5 shape; "lease" = `/dev/cpu_dma_latency` 20 us held by the bench process. labels
reference (M18a-4 tree, same harness, `res-m5c.txt`): facts 0.95/0.97 mean (24/200 ms), CPU 289 % / 48.7 %.

| keys | variant | turn p99 us | facts mean | min plugin | CPU % of a core | guest call p50 us | cold pick-ups (of 400 turns) |
|---|---|---|---|---|---|---|---|
| 24 ms | off (before) | 487 | 0.21 | 0.12 | 37.0 | 91 | 3300 |
| 24 ms | placement | 310 | 0.33 | 0.22 | 35.1 | 69 | 3300 |
| 24 ms | lease only | 312 | 0.93 | 0.90 | 18.3 | 50 | 50 |
| 24 ms | **placement + lease** | **309** | **0.955** | 0.90 | **19.4** | 42 | 210 |
| 200 ms | off (before) | 359 (spike 8422, load 8) | 0.49 | 0.03-0.10 | 19.0 | 118 | 1500 |
| 200 ms | placement | 390 (spike 8424, load 7) | 0.50 | 0.03-0.18 | 18.4 | 107 | 1400 |
| 200 ms | lease only | 337 (2083 once, load 9) | 0.74 | 0.67 | 9.3 | 94 | 18 |
| 200 ms | placement + lease | 371 (599 once) | 0.71 | 0.39-0.56 | 13.4 | 83 | 260 |

Verdict C-4 (facts >= 0.95, p99 <= 310 us, CPU <= 1/4 of labels):

* **Placement alone: not met** (small gain: guest call -20 %, facts +0.1 at 24 ms).
* **Placement + lease, 24 ms keys (typing/navigation bursts): met on the mean** (facts 0.955, p99 309 us
  on the median of three, individual runs 301-314; CPU 19.4 / 289 = 0.07). The weakest plugin is 0.90, below 0.95.
* **Sparse keys (200 ms): not met**: facts 0.71-0.74, CPU ratio 0.19 (lease only) to 0.28 (both). After 200 ms of
  idle the core also drops its frequency (`powersave`); C-states are fixed by the lease, P-states are not (open).
* The probe `lease-probe.txt` shows the kernel value 20 while the lease is held and 2000000000 (no
  constraint) otherwise.

## 4. System changes made for the measurement (reverted)

`sudo setfacl -m u:<me>:rw /dev/cpu_dma_latency` for the lease runs, removed by an `EXIT` trap
(`setfacl -x`); checked afterwards with `getfacl` (owner entry only). The same effect as the udev rule, nothing
persistent. No sysfs, governor or udev file was written. The bench processes closed their fds on exit (lease dropped).

## 5. Open

* Sparse-key facts need the core frequency too (cpufreq `powersave`/EPP): a separate owner decision, not done.
* The lease is system-wide while held (2 s after the last key); power/heat cost is not measured (RAPL needs root;
  PX-13 energy part open).
* `warm_on_arm` diagnostic (default off): placement + warm: guest call 69 -> 67 us, facts unchanged; with the lease it
  adds nothing measurable (not rerun, `res-v5-final-shape.txt` rows `v3-warm`, variant 3 shape).
* Full-workspace test run: see the brief.
