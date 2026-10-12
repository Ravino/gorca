# Phase 1 vertical slice: adversarial review (review-r1)

- Branch `worktree-agent-a91eb163b53acf271`, head `23845ab`, diffed against `main`.
- Reviewer: reviewer agent, 2026-10-13.
- Raw outputs and probes: `bench/results/e2e/review-r1/` (in the main repo).

## Verdict: BLOCK

The latency gates pass with a wide margin, and the build, clippy, fmt, tests and dependency rules
are all clean. Five HIGH findings break the product goals, though:

- stale speech is spoken after a cancel;
- an application can be muted permanently;
- speech plays twice on failback;
- one local process can grow gorca's memory to about 2.4 GiB and lock new apps out.

Each fix is small and local. Re-review after F1–F5 and F6 are fixed.

## Gates (measured)

| Gate | Result |
|---|---|
| `cargo build --release -p gorca-daemon` | ok |
| `cargo clippy --workspace --all-targets -- -D warnings` | ok |
| `cargo fmt --check` | ok |
| `cargo test --workspace` | all pass (`gates-build-clippy-test-deps.txt`) |
| `cargo xtask deps` | `dependency rules: ok (26 workspace crates, 183 packages)`. No core→connector type leak. `gorca-testkit` is a dev-dependency only. |
| `unwrap`/`expect`/`panic!` in non-test code of the diff | none. `gorca-speech/src/fake.rs` is `#[cfg(test)]`. |
| Wildcard `_ =>` on enums in new code | none. The only wildcards are integer or keysym matches. |

### Latency: `bench/e2e/run.sh review-r1 --reader daemon:<release gorca>,--impersonate-orca`

- Commit `23845ab` (clean tree), release build.
- Host load: 8.77 at script start and 13.84 at measurement start (16-CPU 285H, powersave governor).
- Values are the median over 3 repeats; min and max are in the result file.

| Metric | p99 (ms) | Gate | lost |
|---|---|---|---|
| L1 (voicego) | 2.02 (max 2.81) | ≤15 PASS | 0 |
| L1s (SSIP) | 2.15 | ≤15 PASS | 0 |
| L2 | 0.93 | ≤5 PASS | 0 |
| L3-10k | 1.49 | ≤15 PASS | 0 |
| L3-50k | 3.06 (max 5.75) | ≤15 PASS | 0 (median; **one repeat lost 1**) |
| L4 | 1.96 | ≤15 PASS | 0 |
| K1 | 0.61 | ≤15 PASS | 0 |

Two harness caveats:

1. **K1 p50 = 0.000 ms**, and in one repeat p99 = 0. K1 matches key presses to speech by order, so a zero value means speech was matched to a key that arrived at the same moment or later. K1 as measured cannot catch a regression. Tag the echo with the key.
2. **L3-50k lost 1 event in one repeat.** The median gate hides it. `lost` should be gated on max, not median.

The earlier `daemon-r1` result was measured at `1ab8094` with a dirty tree, not at the head under review. `review-r1` supersedes it.

## Findings (ranked)

### F1 HIGH: an explicit Stop can be undone, and stale text gets spoken (voicego)

- **Where:** `crates/gorca-speech-voicego/src/lib.rs:234-241` arms the resend guard for any Speak sent within 100 ms of a STOP. `Stop`/`StopAll` (`:252-258`) never call `tracker.mark_replaced`. `crates/gorca-output-api/src/track.rs:250-260` then answers `703` of a not-yet-started utterance with `Resend`.
- **Scenario:** `lane.rs:415` sends `Stop{B}` a few ms after `Replace(A→B)`, because the newcomer is already stale. B is cancelled before its `701`, the client resends B, and B is spoken after a newer event.
- **Proof:** probe `probes/voicego_resend_after_stop.rs.txt` prints `spoken after explicit Stop: ["first", "stale second"]; resends=1`.
- **Fix:** current voicego main already has the STOP/SPEAK race fix (`refs/voicego` 58f0908, test `TestStopThenSpeakNotLost`), so the resend workaround is now wrong.
  - Remove it, or gate it behind an explicit compatibility flag for old voicegod.
  - In any case, every Stop/StopAll must disarm the guard for everything in flight.

### F2 HIGH: an async `502-<id>` fails the wrong utterance

- **Where:** `track.rs:141-163` ignores `msg_id` on 4xx/5xx. If a SPEAK reply is still expected, that Expect is popped and failed instead.
- **Proof:** probe `probes/tracker_502_with_id.rs.txt` yields `[Accepted(1), Error(2)]` with `live=1`.
  - Utterance 1 never terminates, so the speech state machine waits until its own timeout.
  - Utterance 2's `225` is lost: it is spoken but reported as an Error.
- **Trigger:** the audio device is gone. voicego sends a delayed `502-<id>` (binary framing) for each SPEAK while the user keeps navigating.
- **Fix:** if `msg_id` is `Some(id)` and `ids` contains it, fail that utterance and leave `expects` alone.

### F3 HIGH: moving back from SSIP to voicego causes double speech

- **Where:** `crates/gorca-speech/src/failover.rs:505-507` reports in-flight SSIP utterances as `Stopped` and drops the client. `SsipClient::drop` (`gorca-speech-ssip/src/lib.rs:~501`) only cancels tasks and sends no `CANCEL self`.
- **Why it double-speaks:** speech-dispatcher keeps playing a closed client's queued messages (`refs/speech-dispatcher/src/server/speechd.c:447-450`: "freed ... as soon as the client no longer has any message in the queues"). speech-dispatcher keeps talking over voicego, and gorca reported the utterance as stopped.
- **Fix:** before leaving a backend, submit `StopAll` and allow a bounded flush of about 50 ms, then drop it. The opposite direction is safe because voicego cancels on close.

### F4 HIGH: a failed announce mutes that application for the rest of the drain connection

- **Where:**
  - `crates/gorca-dbus/src/drain/raw.rs:577-590` caches `router.route()` = `None` per sender.
  - `crates/gorca-connector-atspi/src/router.rs:175-183, 209-215` returns `None` on transient errors: `Timeout` (core slower than `ANNOUNCE_WAIT` = 250 ms), `Full`, a full `new_apps` queue, and `Duplicate` while another announce is still in flight.
- **Proof:** probe `probes/drain_transient_route_mute.rs.txt`. The router fails once, then would succeed. Of the 50 later events, **0 were delivered**, and the router was never asked again.
- **When it ends:** the cache entry goes away only on NameOwnerChanged, a drain reconnect, or after 4096 distinct senders (`MAX_ROUTES`). In practice Firefox or Terminal stays silent until gorca restarts.
- **Likely triggers:**
  - The burst of announcements at startup or login while the host is loaded. This host's load average is 11–14.
  - A hostile flood that fills the registrar while a legitimate app appears. A transient cross-tenant DoS becomes permanent.
- **Fix:** make the router contract tri-state, `Route(AppRoute) | NotAnApp | Retry`.
  - The drain caches `NotAnApp` (the registry, well-known names, ignored names) and never caches `Retry`.
  - For `Retry`, keep a per-sender "not before" instant with exponential backoff (10 ms → 1 s, capped), so a hostile sender cannot make the drain block on every message.
  - Count the dropped events. On `Duplicate`, wait for the in-flight announce instead of returning `None`.
  - Add the probe as a regression test in `gorca-dbus`.

### F5 HIGH (security, scan item 1): the per-app path quota is defeated by many bus names; new apps are locked out

- **Where:** `crates/gorca-connector-atspi/src/paths.rs:18-24, 45-47`. The quota is 1024 paths of 512 B per **AppId**, and an AppId is minted per unique bus name. One local process with N connections gets N quotas.
- **Proof:** probe `probes/paths_many_names.rs.txt`, release build: 4096 names × 1024 paths gives **4,194,304 entries, +2381 MiB RSS**. Then `victim_recorded=false`.
  - Once 4096 buckets exist, a newly started legitimate app is refused (`MAX_APPS` refuse-new). Its focus can never be fetched (`fetch.rs:394` returns `Gone`), so it is silent.
- **Fix, in the ADR-0004 spirit:**
  - Key quotas by connection credentials, not bus name. Call `GetConnectionCredentials` once per announce, inside the existing announce deadline; on failure, fall back to the unique name with the smallest quota.
  - Per-uid/pid budget: about 1024 entries per pid in total, shared by all its names.
  - A global cap measured in **bytes**, e.g. 16 MiB across all buckets, not a count of apps.
  - Preemption instead of refuse-new: when the global cap is hit, evict from the bucket of the pid with the largest usage, as in AU-3. A new app always gets a small reserve.
  - Store path bytes once: intern by `Arc<str>` with a byte budget, and lower `MAX_PATH_BYTES` to 256, since real AT-SPI paths are under 64 B.
  - The same multiplier applies to router announcements and app actors (one `AppServer` and two Startup-deadline calls per name). The pid cap should gate `announce` itself.

### F6 MEDIUM: test isolation relies on convention, with no hard guard (item 4)

All current tests pass explicit addresses and sockets:

- `signals.rs` uses `--a11y-address`, `--session-address` and nonexistent sockets, plus `env_remove`;
- `e2e.rs`, `harness_focus.rs`, the `lib.rs` test and the connector test do the same.

I found no test today that reaches the live session. Nothing enforces this, though. Library code still falls back to the ambient environment:

- `connector.rs:101-102` (`GORCA_A11Y_BUS_ADDRESS`, `AT_SPI_BUS_ADDRESS`);
- `session.rs:67` and `gorca-input-mutter/src/source.rs:130` (`Builder::session()`);
- `gorca-speech-ssip/src/lib.rs:110-117` (`SPEECHD_*`, `XDG_RUNTIME_DIR`);
- `gorca-speech-voicego/src/lib.rs:107` (`XDG_RUNTIME_DIR`).

The `cargo test` process itself inherits the user's `DBUS_SESSION_BUS_ADDRESS` and `XDG_RUNTIME_DIR`. Any future test that leaves one `Option` as `None` will talk to the live a11y bus, or speak through the user's voicegod. The harness `Session::a11y_address()` also returns `""` when the key is missing. Today that degrades to the test session, but only by accident.

**Required guard:**

1. Only `gorca-daemon/src/main.rs` may read the environment. Library crates take fully resolved addresses, and `None` becomes an error, never an env lookup. Enforce this with a `cargo xtask` rule that forbids `std::env::var*` and `Builder::session()` outside bin crates.
2. `gorca-testkit` exposes one `isolated_options()` constructor that every test uses.
3. A test-time tripwire: the testkit records the inherited `DBUS_SESSION_BUS_ADDRESS`, `AT_SPI_BUS_ADDRESS` and `XDG_RUNTIME_DIR` at start-up. Every connect path in debug builds asserts that the target is not one of them (`debug_assert` behind a `cfg(any(test, feature = "test-guard"))` hook).
4. `signals.rs` should use `env_clear()` plus an allowlist instead of `env_remove`.

### F7 MEDIUM: the NameOwnerChanged stream can stall the shared call connection

- **Where:** `connector.rs:221-226` creates `MessageStream::for_match_rule(rule, &conn, Some(256))` on the **same** zbus connection that serves every `AppServer` call. The rule has no `arg0`/`arg2` filter, so every name change on the a11y bus is queued.
- **Mechanism:** zbus documents that when a stream's queue is full, "no more messages can be received until room is created" (`refs/zbus/zbus/src/connection/mod.rs:165-168`).
- **When the stream stops being polled:** the connector loop awaits `stop_drain` (up to 2 s), `start_drain`, and the backoff `sleep` (up to 5 s) inside a `select!` arm.
- **Effect:** with 256 or more connects/disconnects during a drain restart, all method replies on `conn` stall and every fetch for every app times out. This is by code reading, not reproduced.
- **Fix:**
  - Use a dedicated connection for NameOwnerChanged, or add `arg2=''`.
  - Move the drain restart and backoff into a spawned task, so the select loop never blocks.

### F8 MEDIUM: the Reader lane dies silently after 5 panics in 10 s

- **Where:** `gorca-core/src/runtime.rs:629-632`. `reader_lane` returns, and `Core::run` keeps waiting on `shutdown.cancelled()` (`:327`).
- **Effect:** the daemon stays up but mute, and nothing is announced.
- **Fix:** when the lane exits other than cleanly, cancel `shutdown` with an error so the process exits non-zero and systemd restarts it. Speak a notice first if the speech lane is alive.

### F9 MEDIUM: the drain thread blocks for up to 250 ms per new sender

- **Where:** `router.rs:170-172`, `Handle::block_on`.
- **Effect:** while it blocks, events of **all** apps sit in the socket. Under a stalled core or a burst of new names, this violates "a misbehaving app degrades only its own actor". The 50k storm did not exercise new names.
- **Fix:** together with F4, register asynchronously.
  - The router returns `Retry`, and the drain parks a small bounded number of events per unknown sender, or drops them and counts them.
  - The announce completes on the runtime, and the next event routes.

### F10 MEDIUM: a Stop is lost silently under write back-pressure (speech)

- **Where:** `gorca-speech-voicego/src/lib.rs:513-523` returns `Full` for a non-supersedable `Stop` when the 4-slot queue is full of speaks. `failover.rs:575-579` drops it without an event.
- **Also:** a plain Speak can supersede a queued Replace and discard its STOP, and superseded utterances get no terminal event.
- **Fix:** a Stop evicts queued speaks, reports them `Stopped`, and always enters the queue.
- **Evidence:** code reading only.

### F11 MEDIUM: one router regression test does not test the fix

- **Where:** `router.rs` `connect_disconnect_churn_leaks_no_routes_or_budget`.
- **Problem:** it **passes on the pre-fix parent `1ab8094`**, so it does not exercise the race.
- **Run on the pre-fix code** (`prefix-1ab8094-regression-tests.txt`): `an_app_dying_mid_registration_leaks_nothing` fails at `router.rs:344`, and `a_hostile_app_cannot_touch_another_apps_entries` and `the_number_of_apps_is_bounded_and_existing_apps_keep_working` also fail. Those three tests do prove their fixes.
- **Fix:** add an interleaving hook, such as a test-only barrier between `block_on` and publication, so the churn test exercises forget-during-announce deterministically.

### F12 MEDIUM: the first request for a new app waits behind `announce_info` (Startup deadline 5 s)

- **Where:** `fetch.rs:322-324` awaits `app_info` before entering the request loop.
- **Effect:** the first focus in a newly started app, which the user is about to hear, waits for two extra round trips. If the app is slow to answer `ToolkitName`, that focus gets Busy or a timeout until the 5 s deadline passes, and `gone` is ignored during that time.
- **Fix:** spawn `announce_info` into `inflight` and serve requests immediately.

### F13 LOW: SSIP reply blocks have no total size cap

- **Where:** `gorca-speech-ssip/src/lib.rs:237-272` caps each line at 1 MiB, but a block of continuation lines (`NNN-`) grows without limit while lines keep arriving within 2 s of each other.
- **Fix:** cap the block at about 64 KiB.

### F14 LOW (security, scan item 3): speech socket peers are not authenticated

- **Where:** the voicego and SSIP clients connect to whatever path is configured or found in the environment.
- **Risk:** gorca deliberately sends everything to that socket, including passwords and terminal output. Anything that owns the socket receives it.
- **Fix:** check `SO_PEERCRED` uid == own uid (via `UnixStream::peer_cred`) and refuse otherwise.

### F15 LOW: `label_target` lets an app direct gorca's calls

- **Where:** `fetch.rs:227-230` calls `Properties.Get(Accessible, Name)` on an app-chosen `(bus, path)`.
- **Risk:** this is bounded by the deadline and is read-only. A malicious app could aim it at a hung peer to eat the focus deadline, but that only slows its own focus.
- **Fix:** restrict `bus` to `self.bus_name` or a known app, or charge the call to the target app.

### F16 LOW: `MAX_DEPARTED` FIFO eviction

- **Where:** `router.rs:44`.
- **Risk:** after 8192 departures, a straggler event from an evicted name can be announced again. It then leaks until no NameOwnerChanged ever arrives for it again.
- **Fix:** the drain's own NameOwnerChanged handling already clears its cache. Purge departed names once the drain has acknowledged them, and make the cap time-based.

### Info

- **Scan item 2 (sensitive text):** no utterance text is logged at the default `info` level.
  - All `info`/`warn`/`error` sites in the diff log bus unique names, error values or constants. App names and toolkit names appear only at `debug` (`fetch.rs:359`).
  - `tracing-subscriber` is 0.3.23, which escapes ANSI in fields, so log injection through app strings is mitigated.
  - There are no `/proc` reads in new non-test code.
  - **Conflict with the stated requirement:** `fetch.rs:258` ("Passwords are never spoken") never fetches `PasswordEntry` text. The orchestrator says the user requires gorca to read everything, so this is a product-spec deviation for the orchestrator to decide. It is not a security issue.
- **Cancel+speak (item 1):**
  - On voicego a `Replace` is one `write_all` (`voicego/lib.rs:335-358`, test `replace_is_one_write…`).
  - On SSIP the protocol forbids it. STOP, SET and the SPEAK header go out in one write, and the body is released on `230`, bounded at 2 s.
  - For focus, the epoch advance sends `Stop` at once and the `Speak` follows the fetch. That is two writes by design (E-2), not a defect.
- **Stale replies:** they are dropped twice, by `runtime.rs:517` and `reader.rs:368`. Stale fetches are not aborted, but they are bounded by a 64-entry global cap and an 8-per-app cap.
- **e2e tests:** they return early (pass) when the harness prerequisites are missing (`e2e.rs:23-28`). CI should fail instead of skipping.

## Rerun list for the next review

1. Copy `probes/*.rs.txt` back into place:
   - `drain_transient_route_mute` → `crates/gorca-dbus/src/drain/raw.rs` tests;
   - `paths_many_names` → `crates/gorca-connector-atspi/tests/`;
   - the two speech probes → `crates/gorca-speech-voicego/tests/` and `crates/gorca-output-api/tests/`.
2. Rerun `bench/e2e/run.sh` and check `lost` per repeat.
