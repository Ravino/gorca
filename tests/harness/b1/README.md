# B-1 harness: fake apps, walk W-1, conformance suite

Bake-off `docs/bakeoff/b1-mirror-fill/SPEC.md` (test-engineer share: H-2, H-3, H-8, H-10).

```text
tests/harness/b1/
  fakeapp/            b1-fakeapp: Rust fake AT-SPI app (crate b1-fakeapp), one binary, many models
  models/*.json       shell settings files files-big gtk3 storm hung ff-chrome (SPEC 8.1, P-9 values)
  walks/w1.toml       the scripted walk W-1 (SPEC 8.2), integers only
crates/gorca-testkit/src/b1/
  proto, service, world   model schema, control commands, truth log; seeded service-time model;
                          the pure state of one fake app (tree, mutations -> events + truth)
  sut, sim                the Sut trait (a reader attached to fake apps) and the virtual-time
                          reference reader implementing SPEC section 5
  real                    RealSut: b1-fakeapp processes + the daemon's PW-10 presentation log
  ic, verdict             IC-1..IC-23 and the in-memory verdict classifier
  audit, items            MA-1..MA-10 over adapter traits; GetItems codec + reference decoder
  walk                    W-1 plan/executor, run-directory writer (contract of gorca-bench-b1)
  selftest                H-10(b)/(c): the suite and the gates must fail for known defects
crates/gorca-testkit/src/bin/b1_walk.rs   the B1_WALK_DRIVER program
```

## The fake app

`b1-fakeapp --model settings --bus $AT_SPI_BUS_ADDRESS [--truth F] [--p2p peer|same_bus|none]`.
It refuses a bus that is not a `/gorca-h.*` harness session (`--allow-any-bus` for the tests'
own dbus-daemon). Control: one JSON object per stdin line (`gorca_testkit::b1::proto::Cmd`),
answers on stdout; the first line is `ready` (bus name, root, peer address, landmarks).

* **Serial service time (D-1).** One thread plays the toolkit main loop: every call from every
  connection (bus and peer) and every control command is served one at a time, in arrival
  order, for a log-normal service time fitted to the model's p50/p99, plus a per-item cost for
  `GetItems`. Test: `tests/fidelity.rs` (client-side p50 within +-35 % + 50 us under load; the
  SPEC gate is +-20 % on a quiet host, H-12).
* **Quirks** (`Quirks` in the model): G-1/S-1 derived names, hidden ghosts, add-during-dump,
  recycled rows, switcher rebuild (S-2), `GetItems` realised-only (P-9), no `RemoveAccessible`
  (H1), spurious parent `ChildrenChanged:add` (P9-2), listener gate (P9-1).
* **P2P modes (D-7).** `peer` is a real unix socket served by the same dispatcher (`ClosePeers`,
  `PeerAcceptDelay` for IC-19), `same_bus` answers the a11y bus address, `none` answers "".
* **Truth log (SPEC 6.1)** `{t_ns, app, path, field, old, new, evented}`, `field` is `name`,
  `value` (number) or `states` (sorted state names); focus changes are `{t_ns, focus:true, path}`.
* **Counters**: per `<conn>/<Interface.Member>`, calls below a `Document*` node, calls to dead
  paths, distinct bus callers, signals sent.
* Hostile input: `Cmd::Malform` plants foreign refs, self-parents and parent cycles.

## Running things

```sh
cargo test -p gorca-testkit -p b1-fakeapp                  # suite self-tests, audits, fake app on a private bus
cargo build -p gorca-testkit -p b1-fakeapp                 # b1-walk and b1-fakeapp next to each other
B1_RUN_KIND=walk B1_RUN_DIR=/tmp/r B1_QUICK=1 target/debug/b1-walk --sim            # reference reader
B1_RUN_KIND=walk B1_RUN_DIR=/tmp/r B1_QUICK=1 target/debug/b1-walk --sim never-invalidate   # a known-bad reader
B1_WALK_DRIVER=target/release/b1-walk bench/b1/run-all.sh <label> --arm a:/path/gorca-daemon
```

`--sim` faults: `none never-invalidate never-cache no-rv1 dump-every-100ms`.

## Run directory

`b1-walk` writes the contract of `crates/gorca-bench-b1/src/rundir.rs`. Paths in both logs are
prefixed with the unique bus name of the app instance (`<bus><path>`), because restarted apps
reuse their paths on purpose (IC-6) and `b1 run` joins by path alone. `app` in the truth log is the
bus name too, so `KeyMode::AppAndPath` agrees.

## Wiring once PW-1..PW-12 land

| Item | What to add |
|---|---|
| MA-1..MA-6 (mirror) | `impl MirrorAdapter for <gorca-mirror store>` in `crates/gorca-mirror/tests/`, call the `run_*` functions of `gorca_testkit::b1::audit`; un-ignore `audit_apply_bounded_real_mirror`, `audit_app_churn_rss` |
| MA-1/6/9 (policy) | `impl PolicyAdapter for Candidate`; un-ignore the `*_candidate` tests |
| MA-7 | `impl ItemsDecoder for fetch::items`; `GORCA_FUZZ_SECS=600` |
| MA-8 | `impl AdmissionAdapter` over the driver's slot accounting |
| MA-10 | run `gorca-daemon` with `GORCA_SHADOW=1`, assert `shadow_offenders(stats.calls)` is empty |
| IC-* on the daemon | the daemon-side MirrorRead probe (SPEC 5.7) -> `RealSut::probe`; `where-am-i` command -> `RealSut::where_am_i`; then `Feature::{Probe,WhereAmI}` become true and the skipped cases run |
