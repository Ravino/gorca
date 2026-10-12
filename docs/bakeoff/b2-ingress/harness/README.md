# B-2 harness: event socket drain + ingress coalescer

One harness measures the reference arm and every candidate. It compiles against the public API
of `gorca_dbus::drain` and `gorca_ingress` (SPEC sections 3 and 4); a candidate replaces the
internals of those two crates in its own worktree and keeps the API, so nothing in the harness
changes between candidates. Contract and pass thresholds: [`../SPEC.md`](../SPEC.md).

```
crates/gorca-bench-b2/            the harness (library + `b2` binary + conformance tests)
  src/wire.rs                     D-Bus wire codec written independently of zbus (ground truth)
  src/workload.rs                 deterministic AT-SPI event generators (5 mixes) + expected mapping
  src/model.rs                    the executable spec: exact ingress model, consumer fold, compare()
  src/mockbus.rs                  in-process broker: SASL, Hello, AddMatch, F-3 quota + disconnect
  src/bus.rs                      bus fixtures: mock, private dbus-daemon, private dbus-broker
  src/feeder.rs                   paced senders, probe stamping, ground truth replay
  src/rig.rs                      router + one consumer thread per app (the "app actor")
  src/runner.rs                   one complete run -> RunOutcome (latencies, CPU, RSS, verdicts)
  src/tap.rs, src/fakeapp.rs      truth tap + Python fake-app scenarios (isolated harness)
  src/scenarios.rs, src/limits.rs benchmarks T1..T7, fixture self-test, and every threshold
  tests/ingress.rs                ingress conformance against the model
  tests/drain.rs                  drain conformance (framing, mapping, routing, robustness)
  tests/realbus.rs                the same on dbus-daemon and dbus-broker, plus the F-3 fixture check
docs/bakeoff/b2-ingress/harness/
  run-all.sh                      conformance + benchmarks + fake apps, one label
  compare.py                      side-by-side comparison of result files
bench/results/b2-ingress/<label>/ result files (JSON + text), comparable across commits
```

## Running

```sh
. ./env.sh
docs/bakeoff/b2-ingress/harness/run-all.sh <label>            # about 25 minutes
docs/bakeoff/b2-ingress/harness/run-all.sh <label> --quick    # about 4 minutes, 1 run, no real buses

cargo test -p gorca-bench-b2                                  # conformance only (debug)
cargo build --release -p gorca-bench-b2
target/release/b2 bench --label X --only t2,t3 --repeats 3    # selected scenarios
target/release/b2 threads --rate 50000 --secs 4               # where does the CPU go?
tests/harness/run-isolated.sh target/release/b2 fakeapp mixed --secs 15 --label X
docs/bakeoff/b2-ingress/harness/compare.py bench/results/b2-ingress/*/2*.json
```

`b2` refuses a debug build (the numbers would be meaningless) unless `--allow-debug` is given.
Run benchmarks on an otherwise quiet machine: every result file records `loadavg_at_start` and
`loadavg_at_end`, and the reviewer re-runs a candidate whose load average was above 4.

## What a run does

```
 feeder thread per app --(paced signals, probes stamped before write)--> bus --> drain under test
     (workload.rs)                    mock | dbus-daemon | dbus-broker             |  spawn_drain()
                                                                                  v
 truth = replay of the generator (or the tap)                          IngressSender::push (per app)
        |                                                                         |
        |   compare(truth, fold)                                    consumer thread per app
        +-------------------------------------------------------<-- notified() -> drain() -> fold
```

* **Probes** are focus events on unique nodes, one every 0.5 to 5 ms, stamped with `now_ns()`
  immediately before the `write` that carries them. The consumer stamps the `drain()` return.
  Latency of probe `k` is the stamp of the first batch that reflects probe `j >= k`
  (focus is last-writer-wins, so a probe superseded by a newer one is reflected by it).
* **Fence**: every feeder ends with one more focus event. A run is *settled* when every
  consumer has seen its fence. A lost fence is a failure.
* **Ground truth**: the generator is a pure function of its seed, so the truth is the replay of
  the first `sent` events folded by `model::Fold`; no truth is computed on the hot path.
  `compare()` allows exactly one deviation: nodes the implementation reported dirty (resync).
* **CPU** is the on-CPU time (`/proc/self/task/*/schedstat`) of every thread whose name does not
  start with `hx-`. Harness threads are all `hx-*`, so the candidate's threads are counted
  whatever it names them. **RSS** is `VmRSS`/`VmHWM` of the process minus the value before the
  drain was spawned.
* **Mock broker**: queues what the reader has not consumed in userspace without limit (like
  dbus-broker) and disconnects the reader when the queue exceeds `MOCK_QUOTA_BYTES` (64 MiB). A
  disconnect is a hard fail of the scenario.

## Fixtures

| Fixture | Reproduces | Notes |
|---|---|---|
| `mock` | F-3 queue-then-disconnect, at exact byte quotas; fragmented writes; hung broker | in process, deterministic, fastest; the primary latency/throughput bus |
| `dbus-daemon` | what `tests/harness/run-isolated.sh` uses | **drops** signals when the reader's queue is full ("Rejected: destination has a full message queue"), so it cannot reproduce F-3 |
| `dbus-broker` | the real F-3 | `dbus-broker-launch --scope system` against a helper `dbus-daemon` that stands in for the systemd bus, listening socket from `systemd-socket-activate`. Needs no systemd user manager, touches nothing on the host |
| existing bus | the isolated AT-SPI harness (`AT_SPI_BUS_ADDRESS`) | used by the fake-app scenarios |

`b2 fixture` (and `tests/realbus.rs::broker_fixture_reproduces_f3`) checks the fixtures against
each other: a reader that does not read is disconnected by dbus-broker after about 1.1 MiB with an
8 MiB pool (roughly pool/7), the same reader on dbus-daemon silently loses signals, and a reader
that keeps up is never disconnected by either. Scale the pool with `BROKER_POOL_BYTES`.

## Scenarios

| Id | Measures | Where |
|---|---|---|
| T1 | sustained events/s before disconnect or backlog; saturation rate | mock |
| T2 | socket to coalescer output latency p50/p95/p99/p99.9/max at idle, 10k and 100k ev/s | mock |
| T3 | focus-event latency under a 40k ev/s storm (4 apps) | mock, dbus-daemon, dbus-broker |
| T4 | CPU: idle, per 10k events, drain-thread share at 100k ev/s | mock |
| T5 | memory and survival: distinct-key flood and lossless flood with a hung app actor | mock |
| T6 | ground-truth correctness for 5 mixes, flooded and not, 3 concurrent apps | mock, dbus-daemon, dbus-broker |
| T7 | cost of one `push`, uncontended and with 4 producers | in process |
| FA | Python fake apps (storm 40k/s, mixed, hung + SIGSTOP) against a tap | isolated harness |
| F3 | fixture self-test | dbus-broker, dbus-daemon |

Workload mixes (`workload.rs`): `fakeapp` (the storm of `tests/harness/fakeapp.py`: children, text,
state, focus 1:1:1:1), `terminal` (VTE-style delete+insert pairs, F-4), `realistic` (skewed nodes,
every event class), `lossless` (Removed/Window/Focus/Announcement heavy), `distinct` (every event a
new key).

## Conformance tests (what fails a candidate before any benchmark)

* `tests/ingress.rs`: 120 random scripts x 4 configurations compared batch by batch with the exact
  model (events, order, flood state, dirty set, stats); no lost wake-up with a concurrent consumer;
  4 producers converge to the truth; pending slots bounded under distinct floods (including the
  lossless classes); lossless classes survive a flood.
* `tests/drain.rs`: every mix against the truth; byte-by-byte fragmentation; a 3 MB message;
  malformed bodies; non-events and unknown senders; route cache; big-endian; match rules; prompt
  `stop()`; reported disconnect; start-up deadline against a mute broker; a stalled consumer does
  not stall the socket.
* `tests/realbus.rs`: end to end with ground truth on dbus-daemon and dbus-broker.

The harness validates itself: `workload.rs` and `tap.rs` each implement the mapping table
independently of `gorca-dbus`, and unit tests require all three to agree on 5000 generated
messages per mix. The conformance tests were mutation-checked against the reference ingress
(property mask overwrite, Removed not kept, focus count, lost wake-up: each is caught).

## Adding or changing a metric

Add the scenario to `scenarios.rs`, its threshold to `limits.rs` and a row to SPEC section 6. Row
ids are stable keys for `compare.py`; do not rename them. Keep every thread named `hx-*`.

## Known limits

* A single feeder thread generates about 1.5 to 2 M events/s; T1 reports `generator-limited`
  rather than a false pass when the drain keeps up but the feeder cannot go faster.
* The mock broker does not model broker-side forwarding cost; dbus-daemon/dbus-broker runs do, and
  their latencies include it, so they carry no latency threshold, only survival and correctness.
* `tests/harness/fakeapp.py` emits at most about 40k events/s per process.
* Python fake-app focus latency is measured against the tap's arrival time of the same message, a
  lower bound of the true socket-to-output latency.
* The harness does not model NameOwnerChanged, app restarts, or the unicast `KeyEvent` signal.
