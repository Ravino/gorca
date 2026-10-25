# M18a-2 review: plugins in the fusion writers, telegram-web (r1)

Date 2026-10-25, reviewer. Branch `worktree-agent-a8189783046a42a6c`, commit `039d9233`,
`git diff main...039d9233` (78 files, +7796). This review read the code, the ADR-0026 r5 notes, the
implementer report, and ran the commands listed under Evidence.

**Verdict: MERGE AFTER FIXES.** With plugins off, the branch still changes the web attach path
(finding P-1). With plugins on, the fail-closed T3 rule and two AC-1/SS-3 rules are not enforced
(S-1, S-2). The PX-6 latency gate is still open. The numbers come from the host's wake latency and
from how the countdown is built, not from a counting bug (section 2).

## 1. Status of the scan findings

| Scan finding | Status | Where |
|---|---|---|
| URL glob crossing host/path | **Fixed.** The matcher is structured: exact scheme, lower-cased host converted to punycode, wildcards only as whole leading labels, default ports, userinfo refused, `*` never crosses `?`/`#`. I read every branch and found no bypass. | `gorca-plugin-host/src/urlmatch.rs` |
| `verified: true` hardcoded | **Fixed.** `verified` comes only from `scope_identity(app_name, exe)`. A `flatpak:` identity is never verified. The app actor adds `verified_shell`, which is fine. The only remaining literal is in the PX-6 test. | `plugin_port.rs:314`, `doc_registry.rs:487`, `app_actor.rs:850` |
| Basename-only exe, mount-ns, flatpak, snap spoofing | **Fixed per the agreed model.** The start time is checked before and after the reads. A native process must share our mount namespace and run a root-owned `/usr` or `/opt` executable with the same dev and inode. `/usr/local` (group `staff`, group-writable) correctly fails. Snap needs an `(enforce)` AppArmor label plus an inode match under `/snap/<name>/`. Flatpak is sandboxed-unverified. The identity is still the basename, which is acceptable because only root can place the file. | `gorca-connector-atspi/src/proc.rs:226-280` |
| Fact text in logs | **Fixed.** Logs carry `value_shape` (kind and length) and the plugin log length only. `FaultKind` has no payload, nothing logs the doc URL, and a test pins this. | `plugins.rs:604,749,768` |

## 2. Latency: why writer-turn p99 is 653 us

The bound is enforced and the wait is counted correctly. `Turn::starting_at(t0)` sets the deadline
from the turn start (`plugins.rs:725`, `call.rs:143`), and the loop breaks at `deadline_ns`
(`actor.rs:569-585`). Snapshot, `new_job` and dispatch happen inside D.

**What fills the turn.** p50 is 390 us. That means most turns run all the way to D, because the
countdown reaches zero only when all ten replies are in, and that happened in 29 % of turns. Every
turn that runs to D then adds the writer's own late wake on top. A probe that copies the
production mechanism (10 parked workers, writer park until D−30 us then spin, timer slack 1 ns,
24 ms spacing) gave these numbers at load1 2.5–3 on this host. That load is much lower than the
load during the PX-6 runs.

| Metric (probe, `scratchpad/px6probe/probe.c`) | p50 | p99 | max |
|---|---|---|---|
| Writer wake after D (park to D−30 us) | 55 us | 141 us | 180 us |
| Slowest of 10 parked workers running after t0 | 182 us | 276 us | 339 us |
| 10 sequential `futex_wake` (the send) | 55 us | 132 us | — |

Cause: this host's idle states are `C2_ACPI` 127 us and **`C3_ACPI` 1048 us** exit latency, all
enabled. The writer parks until D−30 us, but its CPU needs up to 127 us to wake. The 30 us tail
cannot cover that. The workers park after 40 us idle (`actor.rs:58`) and sleep through the 24 ms gap
in deep C-states. At load 11, preemption of the spinning writer comes on top of this. So p99
653 us = D + late wake + preemption. **This is not a counting bug. It is a mechanism that does not
hold under default power management**, which is also the owner's daily configuration.
`bench/b1/quiet-host.sh` disables every idle state above 20 us, so a "quiet host" PASS would hide
exactly this.

**Work outside D that PX-6 does not measure.** In the real writer, `apply_plugin_ops` and the fuse
`run()` happen after the wait (`doc_actor/fuse.rs:566-567`, `app_actor/fuse.rs:790`). ADR r4 puts
fusion inside the 0.3 ms. PX-6 stops at `take_ops`, so it understates the real turn.

**Why first-utterance coverage is 29 % and not 99 %.**
- *Wake fan-out:* the last of the 10 workers starts about 180–280 us after t0, and each derive then
  needs 55–170 us.
- *Busy plugins:* a busy plugin is tagged into the countdown (gap 7, `plugins.rs:720-726`). The
  writer therefore always waits to D for it. ADR §2.4.6 said such a plugin should be excluded from
  the wait.
- *Serial-equivalent grants:* each plugin gets about (300 − spent − 20)/10 ≈ 25 us of fuel. That
  probably fits a Telegram derive in fuel units, but it caps any heavier plugin by design until
  MQ-4.

Per-plugin coverage falls in dispatch order (0.825 → 0.57), which points to wake order, not fuel.
The 99 %/97 % gate at n = 10 cannot be reached with parked workers on this host's default C-states.

Fixes, in order of value:
1. **Pre-wake on key-down.** The keypress reaches gorca milliseconds before the focus change.
   Arming the scope's workers to spin for about 50 ms hides the whole wake fan-out.
2. Make the writer **spin the whole window** instead of parking. This costs at most 0.3 ms per 20 ms.
3. Wake all workers with **one** futex word and a single `FUTEX_WAKE(INT_MAX)` instead of 10
   syscalls.
4. Leave busy plugins **out of the countdown** (send them untagged).
5. Run PX-6 on the default power configuration and include apply + run in the measured turn.

## 3. Findings (ranked)

### S-1 High (plugins on): T3 plugins run with no system-scope classification; fail-closed is violated
`plugins.rs:416` `system_scope()` reports only `SecretNode`. Authentication, ShellDialog and GorcaUi
are never produced. `PluginRuntime::load` still loads every package in
`$XDG_DATA_HOME/gorca/plugins` as Untrusted (`plugins.rs:189-196`). It does so with no
`system-scopes` package, no per-plugin enable and no spoken warning.

ADR §2.7 (normative) says: "if it fails to load, no T3 plugin runs."

Scenario: a sideloaded K1 plugin with `[match] apps = []` (matches every verified app) or
`apps = ["gnome-shell"]` opens on the Shell scope (`verified_shell`). A `[[watch]]` on an attribute
the node has then gives it the polkit and keyring dialog rows (minus the password node) and lets
it add S4 facts there. With `TrustedThirdParty` (not reachable yet) the facts would be S1 renames.
Today the tag/class/xml-roles attributes are sparse outside web content, which limits the reach,
but the rule exists so this does not depend on attribute luck.

Fix: refuse `Source::Sideloaded` in `load_with_pins` until M18a-3 ships `system-scopes`, or allow
it only behind `--ext-dev`. The PX-6 dummies can load through a test-only pin list.

### S-2 Medium: secret nodes are targetable, and T3 code runs on a secret focus
- `ScopeRunner::watches` (`plugins.rs:673`) ignores `row.secret`.
- `PluginBridge::interactive` derives a focused password node (`cause::FOCUS`).
- `admit_done` accepts a fact whose `node == target` (`plugin_port.rs:1109`), so a fact on the
  secret node is admitted.
- `row_of` copies the secret node's `text` (`doc_actor/fuse.rs:131`). Every edit therefore
  re-stamps it, and the token that targets it is invalidated and re-derived on each keystroke.

Scenario: a plugin watching `tag="input"` can rename a password field (FS-8 violation). It can also
count its re-derives to learn the password length and keystroke rhythm (the SS-3 rule "no T3 code
on a secret focus" is violated).

Fix, three lines: return no watches for a secret row, reject effects whose node is secret, and
leave `text` out of a secret row.

### P-1 Medium (plugins OFF): unconditional DocURL round trip before the first block
`source.rs:735` awaits `Document.GetAttributes` with a **1 s** deadline before building every web
document, whether plugins are on or off. Scenario: the browser is slow or hung. Document attach
then waits up to 1 s more before the block fetch even starts, and every page load pays one extra
D-Bus round trip.

Fix: fetch only when `wants_url` (pass it through the factory), or run it concurrently with the
first block (`join`).

Also on the plugins-off path, with low impact: `exe_of_async` runs `spawn_blocking` (250 ms cap)
before every app announce (`fetch/mod.rs:302`). The fused-Article ARTICLE flag
(`translate.rs:299,1058`) and the Patch/User overlay guard (`present/node.rs:363`) do not change
output when there are no patches. On main no writer produces Patch or User provenance.

### C-1 Medium: a masked plugin is never unmasked
`Unmasked` is produced only in `poll()` when a reply arrives (`plugins.rs:821`). But the bridge
sends no job to a masked plugin: `plugin_port.rs:803` skips it and `:852` clears its pending set.
The slot leaves `Backoff` only on `CallNeeded` or `BackoffExpired` (`slot.rs:346-367`), and the
daemon never raises `BackoffExpired`.

Scenario: three faults in a window put a plugin into Backoff, and it then stays masked for the
lifetime of the scope (the whole app for K1). Unmask also re-activates the old S4 facts, which were
not retracted on mask.

Fix: the bridge keeps the pending set, and the runner raises `BackoffExpired` from `poll()` (or
from a timer) when `until` has passed.

### L-1 Low: the URL is cached at attach and reused after every Reset
`source.rs:545` re-reports `self.url`, which is fetched once per source. If an engine keeps the
root object across a cross-origin navigation and emits Reset, the scope reopens with the old URL.
This is not verified live. Fix: re-fetch on Reset.

### L-2 Low: PF-1 is overclaimed
Gap 11 says PF-1 is enforced at run time, but instance linear memory persists between `derive`
calls. A plugin can keep counters, so its output is not a function of the view. Reword the claim,
or reset the instance per call for T3 once `TrustedThirdParty` exists.

### L-3 Low: test and tooling hygiene
- PX-6 and `plugin_runtime.rs:31` run `tools/build-plugins.sh` from `cargo test`, and that script
  rewrites the tracked `plugins/pins.txt`.
- `GORCA_PLUGINS_URL_EXTRA` is read in production builds (it is dev-only by intent).
- Host-target clippy of `plugins/telegram-web` fails (7 dead-code errors; the guest is cfg-gated).
  The wasm-target clippy is clean.
- `package.rs` says "case-insensitive", but the comparison is exact.
- `host_call_loops_are_bounded_by_the_grant` failed once in 3 full debug runs (`runtime.rs:684`,
  wall ≥ 40 ms) and passed 5 isolated reruns. Load was 8–20 from other agents.

## 4. Correctness checks

- **PF-2:** enforced structurally. Rows come from S0 (`row_of`), and `RowsView` reads only
  `FrozenRows`.
- **PF-4:** admission enqueues nothing. A job starts only on `upsert` change, attach, focus without
  a live fact, or a requeue after stale, failure or displacement.
- **MQ-2 / PF-7:** checked at admission. A reply is stale if any node of R (targets included) has
  `stamp > t`, and overflow falls back to the scope stamp. Snapshots are taken after the turn's
  upserts in both writers, so a same-turn `stamp == t` is not a false accept. `slot_epoch` drops
  replies from dead incarnations.
- **Starvation:** no plugin is skipped while busy, the queue is newest-wins, and the displaced job
  is requeued. The ten-plugin test delivered 250 of 250. The only exception is C-1. A target that
  churns faster than one derive livelocks under PF-7; that is inherent and acceptable.
- **No Telegram code in the core:** confirmed. The core diff names no Telegram classes or words.
  The article flag and the overlay guard are generic. All selectors, the class table and the
  wording live in `plugins/telegram-web`.

**i18n ("unread"), minimal fix.** WIT 0.10 already has `msg-ref` (`types.wit:78`) for output.
1. Let `facts.assert` take `text::msg(msg-ref)` for text groups, with the count as an argument so
   Russian plurals work.
2. Resolve it on the writer at admission from the package's own catalog in the current locale
   (`to_write`).
3. Make a locale change a source-epoch bump (trigger rule (d)), so PF-1 still holds.

Telegram wording stays in the plugin. Passing a plain locale string into `rule-ctx` would force
every plugin to implement plural rules.

## 5. Merge readiness with plugins off

This depends on P-1. Without it, the plugins-off path differs from main only by inert code
(`plugins: None`) and the `exe_of_async` /proc read. With P-1 fixed, MERGE is safe for
plugins-off users. S-1, S-2 and C-1 must land before anyone sets `[plugins] enabled`.

## Evidence

- `cargo test -p gorca-plugin-host`: 44 + 22 passed in 2 of 3 runs. One run failed
  `host_call_loops_are_bounded_by_the_grant` at `runtime.rs:684`.
- `cargo clippy -p gorca-plugin-host --all-targets -D warnings`: clean.
- `plugins/telegram-web`: `cargo test` 7 passed. Wasm clippy is clean, host clippy fails. The
  built `plugin.wasm` sha256 matches `pins.txt`.
- Probe: `/tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/px6probe/probe.c`
  (`./probe 24000`, `./probe 2000`), load1 2.5–3.0, default idle states
  (POLL/C1/C2/C3 exit latency 0/1/127/1048 us, none disabled).
- gorca-core and gorca-daemon were not built (out of the allowed scope). C-1 and S-2 are from code
  reading.
