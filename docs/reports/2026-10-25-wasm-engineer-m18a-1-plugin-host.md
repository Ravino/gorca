# M18a-1: `gorca-plugin-host` (first slice of the production plugin runtime)

Crate: `crates/gorca-plugin-host` (not wired into the daemon). Test guest: `crates/gorca-plugin-host/tests/guest`
(own workspace, wasm32-wasip2, behaviour chosen by the `cause` code; built by `tests/build-guest.sh`).

## What exists
- Call model B' (owner decision 2026-10-25) behind `call::WLaneRunner`; `actor::ActorRunner` is the only
  implementation: one OS thread per plugin, interactive + background job slot, displacement answered
  `Cancelled`, futex wait on a countdown word with absolute CLOCK_MONOTONIC deadline `D = t0 + 0.3 ms`,
  timer slack 1 ns, supervisor that orphans a wedged actor (`ActorLost`), warm-up of instances off the writer turn.
- `gorca-rt::wait` (new, the only new `unsafe`): futex wait/wake, timer slack.
- Engine: fuel on, epoch off, `Config::target(host)` for the portable arm; ResourceLimiter (16 MiB default,
  4 MiB growth per call); hostcall fuel 64 KiB so an oversize lift is refused before the copy.
- `.cwasm` cache: dir 0700 + user-owned + not a symlink (rechecked on every use), HMAC-SHA256 with a per-install
  secret (0600) verified in constant time before `Component::deserialize`, key = sha256(package sha ||
  compatibility hash), tamper = remove + recompile (miss, not fault).
- Handles: `index(24) || gen(16) || SipHash-2-4 tag(24)` under a per-instance CSPRNG key.
- Packages: manifest, caps default none, sha256 pin list (compiled in, empty until P-0 exists), tiers, reserved ids,
  SS-1..SS-4 (`trust::tier_may_enter`), size caps from metadata before reading.
- `slot::Slot`: enum state machine, every (state, event) cell an explicit arm, `slot_epoch` and stale replies.

## ADR gaps hit
1. wasmtime bindgen enables every `@unstable` feature, so the generated `app-module` requires the `present`
   export. The host uses an inline world `rules-host` (app-module minus `present`) until M18a-6.
2. No typed error for an exhausted hostcall budget in 49.0.3; classified by message text (pinned by a test).
3. A component is several core instances: the limiter needs instances <= 16, tables <= 8, memories <= 4 (ADR says 1).
4. Backoff/Disabled slots need a `Skip` path and the ADR reply set has no `Skipped`; the writer decides it in
   `Slot::event(CallNeeded)`.
5. Manifest cap, manifest format (`[plugin]`, `[permissions].grants`) and per-field text caps are not in the ADR;
   chosen here (64 KiB manifest).
6. Cancel of a *running* job (cooperative trap at the next import) is not implemented; only queued jobs cancel.
7. No WASI random/clock for guests (`define_unknown_imports_as_traps`): a guest using `HashMap` traps. ext-host
   links pure WASI; the plugin host does not yet.
8. `site` world, R lane, `present` export, pool of actors sharing a thread, MQ-3 progressive filling and MQ-2
   token admission are later slices (the runner only carries the grant and the slot epoch).
9. Interactive pre-warm of derive code paths (first-call page faults) is not done; only the instance is built.

## Review fixes (r2, docs/reports/m18a-1-review.md)
- F-1: imports charge fuel before working (`limits::cost`, provisional: base 2000, 1/byte text, 800/child,
  700/handle check), applied to the store fuel by a `call_hook`; 4x-grant watchdog every 64th import.
  Release, heavy view, 0.2 ms grant: text loop 183-203 us (was 28-37 ms), children loop 116-126 us (was 67 ms);
  cold first call ~316 us.
- F-2: limiter totals over all memories/tables, one memory allowed. F-3: lifetime forgery budget 256 (Malformed,
  strike, rebuild with a fresh key), anchors capped; 64-bit tags not done.
- F-4: `ModelView::system_scope`, refused in `node()`/`mint()` and in the dirty-batch match path for third parties.
- F-5: `Event::Cancelled`. F-6: `fsx::open_regular` (O_NOFOLLOW|O_NONBLOCK, fstat regular). F-7: dead actor
  fails closed, runner-owned tags, finished threads pruned, bounded shutdown. F-8: index >= 2^24 not minted.
  F-9 partly: short secret regenerated, cache I/O errors fall back to uncached compile; dir fd/openat not done.
