# ADR-0026 review r1 (WASM plugin model, WIT 0.9 draft)

Date: 2026-10-23. Reviewer: reviewer (opus). Target: main `ae0a6d2d`,
`docs/adr/0026-wasm-plugin-model.md` r1, `wit/gorca-ext-0.9/{types,host,plugin}.wit`.
Method: read-only (no build, per task). Facts checked against `refs/wasmtime` and the pinned
`wasmtime-environ-49.0.3` / `wasmtime-internal-cranelift-49.0.3` sources in `.toolchain/cargo/registry`.
Numbers marked *estimate* were not measured and must become probes (§4).

**Verdict: ACCEPT WITH CHANGES.** The direction is right: facts not speech (PM-1), data before code,
S0-only reads in the writer lane, default-none capabilities, no network. But the latency case for C-3
rests on fuel being a wall-clock proxy, which it is not, and the trust model has two holes that let a
third-party plugin relabel controls plainly and read typed keys. These must be fixed in the ADR text
before M18a-1; C-2/C-3 need the measurements in §4 before M18a-2 is accepted.

## 1. Verified facts

- Bulk memory ops ARE fueled per byte in the pinned 49.0.3 (`VariableOperatorCost`:
  `memory_copy/fill/init_per_byte = 1`, `memory_grow_per_page = 1`; `pre_translate_bulk_op` in
  cranelift 49.0.3 `func_environ.rs:2613,2823,3062,3097`). So `memory.fill`/`copy` loops are NOT an
  escape. Good; the ADR should cite this, it is a load-bearing assumption of C-2.
- `memory.grow` costs 1 unit per 64 KiB page; touching a fresh page costs one store (1 unit) but a
  page fault in the kernel. Fuel does not see faults.
- Fuel charges ~1 unit per wasm operator regardless of cache/TLB misses (`default_operator_cost`,
  `crates/environ/src/tunables.rs`). The 22 units/µs figure is one ALU loop (spike §4).
- Current `gorca-ext-host` enables fuel AND epoch (`crates/gorca-ext-host/src/wasm.rs:236-237`).
- Canonical-ABI lifting of host-import arguments (strings, lists) runs in host code before the import
  body; it consumes no guest fuel.
- No keymap restriction exists in ADR-0019 or ADR-0026 on which chords a plugin may bind, except
  I-7 reserved chords.

## 2. Findings (ranked)

### H-1 Third-party declarative `[[patch]]` writes S1 plainly (UI redress). ADR §2.6 table row 1
"manifest `[[patch]]` (declarative) | S1 | any enabled plugin | plain". A sideloaded K0 (no code at
all) can rename "Cancel" to "OK" or "Delete" to "Archive" at S1, which alt. F explicitly rejects for
computed patches. Combined with an app matcher that matches `gnome-shell` or the polkit/gcr prompter,
it relabels system authentication dialogs.
**Required:** T3 declarative patches go to S4 (hedged) exactly like computed ones unless the user
marks the plugin trusted (QP-3 applies to both). Add a reserved-scope list that T3 matchers can never
match: GNOME Shell system modal dialogs, polkit agent, gcr prompter, gorca's own UI. Same rule for
`hide` (CR-4: say explicitly that `hide` is a patch-layer effect and therefore T2/trusted only).

### H-2 Keymap = keylogger and key hijack. §2.7 CR-6 + `commands.run`: a plugin may bind any
non-reserved chord in its scope. Binding `a`..`z`, digits, `Enter` reveals every typed character by
command id (the command needs no permission), and `observe-typing` "not in 0.9" is bypassed. With
`act` (T3 per-plugin consent) the plugin can also turn Enter into `activate` on another node.
Repeats prior finding F-2 (terminal keylog) in a new form.
**Required:** T3 keymaps may bind only chords that include the gorca modifier, or unmodified keys in
browse mode only; never while focus is in an editable or secret node (AC-1); printable keys and
Enter/Space/Tab/arrows in focus mode unbindable for T3. Add a capability-denial test per rule.

### H-3 C-3 worst case is not bounded by the stated mechanisms (fuel is not wall time). The ADR
claims B-2 adds ≤ 0.3 ms (worst 0.6 ms) per focus change and gate PX-4 "≤ budget + 50 µs". Escapes
not covered by fuel, by import charging (B-8) or by the import watchdog (which only runs inside
imports, every 64th call):
1. **Page faults.** A guest striding 4 KiB stores over fresh memory spends ~3-4 units per page;
   0.2 ms of fuel (≈4,400 units) touches ≈1,100 pages ≈ 4 MiB; at ~0.5-2 µs per zero-fill fault
   (RT kernel) that is ≈ 0.5-2 ms wall, no import, no fault, no strike (*estimate*). B-1 (1 ms fuel)
   reaches the 16 MiB cap: ≈ 2-8 ms per background batch. Repeatable after every instance rebuild
   (pool decommits memory).
2. **Cache/TLB misses.** A pointer chase over 16 MiB costs 1 unit per load but ~80-120 ns per miss:
   4,400 units ≈ 0.4-0.5 ms per interactive call, never a fault (*estimate*).
3. **Argument lifting.** A string in a data segment (CoW image, free at instantiate) passed to
   `diag.log`, `facts.assert(text)`, `attr(key)`, `settings.set(words)` or `output.say` is copied
   and UTF-8-validated by the host before the import runs and before B-6/B-8 can reject it:
   16 MiB ≈ 2-5 ms per call (*estimate*). Return values (`present.edit -> list<ir-edit>`) likewise.
4. **`.cwasm` code page-in.** If deserialized via mmap, the first execution of a code page on the
   focus path is a file-backed fault, a major fault under memory pressure (30 GB host shared with
   the desktop): unbounded disk wait.
5. **Aggregate.** B-2 "0.3 ms across all plugins" is checked between calls; per-call fuel is not
   stated as `min(0.2 ms, remaining aggregate)`, and `attach()`/`removed()` have no budget at all
   though `attach` can run lazily on the focus path after `Backoff -> Cold`.
Slow-but-not-faulting plugins are never struck (prior F-1 stays open). All of this runs on a tokio
worker inside the app actor; for P-4 (`gnome-shell`) that actor is the desktop.
**Required:** (a) wall-clock accounting around every guest call (two `Instant::now()`, ~40 ns) with
strikes for repeated wall overruns > 2× budget, not just fuel; per-plugin adaptive fuel = budget ×
measured units/µs (p10) so slow code shapes get less fuel; (b) per-call fuel = min(per-call,
remaining aggregate); (c) budgets for `attach` (counted against B-1, never on the interactive slice:
attach eagerly at scope attach) and `removed`; (d) cap string/list lengths before lifting: host
imports take lazy `WasmStr`/`WasmList` (or equivalent) and reject over B-6 limits without copying;
cap component data-segment size and package size in the manifest validator; (e) deserialize `.cwasm`
from a read buffer (spike measured that path) or `MAP_POPULATE`/`mlock` code; (f) PX-4 fault
injection must include probes 1-4 and N=10 plugins at full budget.

### H-4 Selector evaluation view is unspecified: a real feedback path. CR-8 covers `model.*`
reads only. `[[watch]]` selectors, `model.matches/select`, and `fact-target::selector` targets are
evaluated by the native selector engine; the ADR does not say on which view. If on fused entries,
plugin A's S1 name patch makes plugin B's watch/selector target match, B's facts depend on A's output,
and a selector-targeted `hide`/`category` can depend on another plugin's patch: order dependence and
possible oscillation, violating MQ-1's premise. Same for `children`/`subtree` if `hide` alters
fused structure.
**Required:** state that in the W lane and in admission all selector evaluation, structure
(`parent`, `children`, `subtree`) and selector-target resolution run on the observed S0 view; watch
dirty causes are S0 changes only (the `cause-code` list already is; make it normative). Add an F-test
with two plugins whose patches would match each other's selectors.

### H-5 W-lane `requests.refresh` self-loop. `requests` is imported by the whole `app-module`
world; nothing forbids a W instance from calling `refresh` in `derive`. `refresh` → S0 change →
cause `refresh` → `derive` → `refresh`... a self-sustaining D-Bus load on the app at ≤ 8 in flight,
never a fault. Also `requests.text` replies go to the R instance, which cannot assert facts, so a
plugin needing uncached message text can never produce the fact (Signal P-2 risk).
**Required:** either forbid `requests` in the W lane, or allow only `refresh` with at most one per
(node, S0 revision); define how a reply reaches the W lane (re-dirty cause `refresh` with the text in
the cache) so request-derived facts are possible.

### M-1 Unprompted, unattributed speech. T3 with `speak` can `say(literal, notification)` from
`reactor.tick` (timers, ≥ 50 ms) without any user action: it can announce "Session locked, enter
your password" in gorca's voice. **Required:** T3 output outside a command is prefixed by the add-on
name or a distinct add-on cue/voice (setting, on by default), rate-limited (e.g. 1 utterance/2 s),
and never `interrupt`; `cue(name)` limited to core cues and declared theme files.

### M-2 Effect caps contradict batch size and atomicity. B-6 facts ≤ 32 per call vs `derive` batches
of 64 nodes: a one-fact-per-row plugin (Signal names) silently loses half its facts. `facts` says
"all or nothing" while B-6 says "excess dropped, counted". `present.edit` comment says 0.2/0.5 ms,
B-3 says 0.1/0.3 ms. **Required:** facts cap ≥ batch size (e.g. 4 × 64) with an explicit choice:
over-cap = whole call rejected + strike (atomic), not partial; align the WIT comments.

### M-3 Handles are forgeable/enumerable. `node-ref = u64`, "never reused", no `resource`. A guest
can guess handles of unwatched nodes and read role/states of any node in scope, contrary to the
"(none): watched nodes" grant; `structural-anchor{path,name: u64}` and `region-anchor{capture}` are
hashes the guest cannot compute honestly, so they are either unusable or forgeable; `basis` is
guest-supplied, so a plugin may pass an older token that read fewer nodes and weaken stale-on-arrival.
**Required:** handles are keyed-random per instance (or index + generation + MAC); reads of
non-watched, non-reached nodes return `none`; anchors are minted by the host (`anchor-of(n)`
accessor); drop the `basis` parameter in W-lane effects (host uses the call token).

### M-4 Interactive slice starves by plugin id. Call order is (plugin id, batch); when the aggregate
is exhausted the "remaining plugins" are always the same ones, so their facts are never at first
utterance and every focus produces a refine (double speech). MQ-3 is open but the default is unfair.
**Required:** rotating or deficit-round-robin order from day one; PX-6 must report the
facts-at-first-utterance rate per plugin, not overall.

### M-5 Milestone dependencies understate M22b. `output.say(node(n, reason))` with browse-line,
quick-nav, review reasons, and PX-8 (quick nav by plugin category) need M22b2/M22b3 (browse/quick
nav/review switched to `Semantics`, phase-3.md:483-484), not just M22b1. **Required:** M18a-5 gate =
M22b2 merged (M22b3 for review reasons), or restrict 0.9 `reason-code` to focus/change until then.
M18a-6 lacks acceptance for the spoken permission review; M18a-2 must name the probes of H-3.

### M-6 Identity and tiers. T3 may reuse a first-party id (`vscode`): collides in CR-2 tie-break,
`ext.<id>.*` settings, diagnostics (prior F-4). `--ext-dev DIR` tier unstated. A first-party file
whose hash no longer matches is unspecified (refuse vs demote to T3). **Required:** reserve
first-party ids; dev mode = T3 always; hash mismatch under `/usr/lib/gorca/addons` = refuse + notice.

### L-1 `SlotState` table vs enum. `Ready`/`Cold` carry no `level`, yet transitions read and keep it;
`Cold` + instantiate failure has no target state; `Backoff` "call ok" is "—". Model the level in a
per-slot record and add the missing rows (CLAUDE.md: exhaustive state machines).

### L-2 Settings and clock in W lane break MQ-1's premise. `settings.get` and `diag.now-ms` make
W output depend on more than S0 history; a settings change never re-derives. Bump the plugin source
epoch on `ext.<id>.*` change (re-derive) and hide `now-ms` from the W lane.

### L-3 Backend neutrality nits. `attr` key `xml-roles` is an AT-SPI/Gecko name (use
`aria-role`); `rect` comment is GTK4-specific; `app-info.name` must be defined across connectors
(AT-SPI app name vs Wayland app_id under Newton); side-by-side AT-SPI + Newton for the same app gives
two scopes and duplicate plugin facts: define the scope as per application, not per connector.
Otherwise no connector type leaks; the API is Newton-compatible.

### L-4 Doc defects. §2.3.2 is cited in WIT but does not exist; "20 accessor calls ≈ 0.5 µs" uses the
26 ns trivial import, string accessors cost ~0.5 µs each (spike §2 ten-string record); alt. B is
priced with `spawn_blocking`, a strawman (see §3).

## 3. C-3 and C-2 positions

- **C-3:** sync `derive` is acceptable only with H-3 (a)-(f). The bake-off arm B in §5 is a
  strawman; the real alternative is **B'**: W instance on a dedicated plugin thread per scope (or per
  plugin), the writer sends the interactive batch and waits on a futex with a 0.3 ms wall deadline,
  publishing without the facts on timeout (refine via LB-4). B' bounds the writer stall by wall
  clock regardless of page faults, misses or lifting; cost is one cross-thread wake (~5-20 µs,
  *estimate*) per focus change. Run the bake-off A vs B' on the Signal fixture with the H-3
  adversarial plugins; criteria: writer-turn p99/max under adversarial plugins, facts at first
  utterance, Δp99 keypress→speech.
- **C-2:** fuel-only is fine for compute; it is not a wall backstop. Keep fuel-only on the engine,
  add wall accounting (H-3a). Epoch adds nothing against page faults inside a single store either
  (it is checked at loop headers like fuel), so only B' or strikes actually bound wall time.

## 4. Required before acceptance

1. Probes in `ext-test` (M18a-2 PX-4, or a spike now): fresh-page stride, pointer chase 16 MiB,
   data-segment 16 MiB string to `diag.log`, 100k-element `present.edit` return, cold `.cwasm`
   mmap page-in with dropped caches, N=10 plugins at full interactive budget. Report wall vs fuel.
2. C-3 bake-off A vs B' (not B).
3. WQ-1..WQ-3 (batch call cost, fuel calibration per code shape, import charging cost) before B-1..B-5
   numbers are frozen.
4. Math MQ-1 must take H-4's selector rule as an axiom.
