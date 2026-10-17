# Add-on platform: security and stability review

Status: reviewer report, 2026-10-17. Read-only review of `main` at `b0caf12b` and of the Telegram Web
demo branch `worktree-agent-aa50a60757c1de84c` (`98cb2344`). Companion to the architect's
`docs/reports/addon-platform-architecture.md` (in progress in parallel; not read).

Scope: `crates/gorca-ext-host`, `crates/gorca-ext-api` (+ `wit/gorca-ext.wit`), the core hook points
(`gorca-core/src/ext.rs`, `reader.rs`, `terminal.rs`, `app_actor.rs`), `extensions/ext-vte`,
`extensions/ext-gnome-shell`, `tests/ext-hostile`, `docs/design/extensions.md`, ADR-0008, the
decisions on the CPU-time backstop (2026-10-16) and speech fairness (2026-10-16), and NVDA's add-on
store (`refs/nvda/source/addonStore/`, `addonAPIVersion.py`).

Probes (scratchpad, not in the repo): an i18n cross-namespace probe (`gorca-i18n` as a path
dependency) and a loader probe reproducing `host.rs` `load_one`'s `metadata` + `fs::read` sequence on
a FIFO and on a `/dev/zero` symlink. Host load average 17 to 21 during the probes (no timing claims
depend on it).

---

## 1. Summary

The M5 host is a good **sandbox for buggy first-party policy code**: no ambient WASI, a narrow copied
model, password-field names withheld, fuel + CPU-time backstop, bounded lifts, per-slot strikes, chords
that can never shadow core or user bindings, catalog namespaces enforced. It is **not yet a platform
for untrusted third-party code**. Its trust model is implicit ("whatever is in the extension directory
is ours"), and four things break once that assumption goes:

1. **Latency has no aggregate budget.** Every inline hook (`present`, `policy`, `terminal.step`,
   `web.describe`) gets its own 2 ms (terminal: 16 ms) CPU allowance, and the host loops over all
   matching extensions. Ten compliant add-ons can legally spend 20 to 60 ms of Reader time on every
   focus change; one compliant terminal policy can spend 16 ms on every key. A slow add-on that
   never faults is never penalised.
2. **The capability list does not cover the dangerous powers.** `capabilities` gates only
   `host.speak`, `settings` and `fetch`. Replacing what the reader says (`present`, `rewrite`,
   `web.describe`, live rules), receiving every raw key and the whole screen (`terminal`), and
   attaching to every application (empty matcher) need no grant.
3. **Identity is a self-chosen name.** The manifest name is the KV namespace, the catalog namespace
   and the command prefix, duplicates resolve by directory sort order, and `verified_shell` is a
   matcher key any manifest can set. M18 plans "user directory with precedence over system ones",
   which turns this into a takeover path for `ext-gnome-shell` and `ext-vte`.
4. **Shared resources fail across add-ons.** One pooling allocator (512 instances), one memory budget
   (none global), two command workers, and one settings file are shared, and pool exhaustion strikes
   the add-on that happens to attach next, not the one that filled the pool.

None of these is exploitable today: only first-party components ship and `--ext-dir` is
operator-controlled. All of them must be closed before M18 lets third-party add-ons in.

---

## 2. Threat model

### 2.1 Assets

| Asset | Why it matters in a screen reader |
|---|---|
| Everything on screen | The reader sees all of it: messages, mail, documents, terminal output (keys, tokens, `cat ~/.ssh/...`). By user decision (2026-10-13) it also reads password fields aloud. |
| Every key the user types | Keys reach the reader (Mutter KeyboardMonitor). The terminal policy gets them raw, passwords included. |
| What the user hears | The user's only view of the screen. Wrong speech is a wrong screen. A blind user cannot cross-check visually. |
| Latency and liveness | p99 < 15 ms key→speech; a muted or slow reader leaves the user locked out of the computer. |
| Add-on state | Settings KV; later API tokens for network add-ons. |
| The user's trust in the store | One malicious popular add-on compromises every user who installed it. |

### 2.2 Adversaries

| Actor | Capability | In scope |
|---|---|---|
| A1 Buggy add-on | Loops, panics, leaks, floods by accident | Yes (today's model) |
| A2 Malicious add-on author | Arbitrary WASM within the WIT world; social engineering in the description | **Yes, new** |
| A3 Compromised legitimate author / malicious update | As A2, but arrives as an update to a trusted add-on | **Yes, new** |
| A4 Store or CDN compromise | Serves any bytes to every user | **Yes, new** |
| A5 Hostile web page or app | Controls the text the add-on reads (prompt-injection-like steering of add-on logic, catalog-marker injection) | Yes |
| A6 Other same-uid process | Can ptrace gorca, rewrite `~/.local/share` | Out of scope (accepted 2026-10-13: same-uid code already owns the session); but **the store must not make A6 worse** (e.g. a writable user directory that silently overrides first-party add-ons) |

### 2.3 Threats

| ID | Threat | Example |
|---|---|---|
| T-1 Exfiltration | Add-on sends what it sees off the machine | A "Telegram helper" with network access uploads every chat it describes |
| T-2 Keylogging | Add-on records typed keys | Terminal policy add-on stores the `sudo` password typed at a prompt |
| T-3 Speech spoofing | Add-on says something false | "Cancel" spoken for a "Delete account" button; fake "update available, press Enter" |
| T-4 Speech suppression | Add-on hides important speech | Silences an error dialog, a security prompt, or all focus speech in one app |
| T-5 Speech hijack / flood | Add-on interrupts or drowns the user | Unsolicited `Interrupt` speech 10 times a second |
| T-6 Latency DoS | Add-on spends the latency budget | Ten add-ons each spending 1.9 ms per focus change |
| T-7 Resource DoS | Memory, instance pool, worker threads, disk | 32 MiB per (add-on, app), across 30 apps |
| T-8 Cross-add-on interference | One add-on overrides, starves, or reads another's state | Name collision takes another's settings; first-match ordering steals the terminal |
| T-9 Persistence abuse | Storage used as a covert stash or for fingerprinting | Keys stashed in KV until a network capability exists |
| T-10 Supply chain | Store, signing key or update channel compromised | Malicious update adds `terminal` + `network` silently |
| T-11 Startup DoS | Package content that hangs or crashes the loader | FIFO or `/dev/zero` as the module |

---

## 3. What is already right (verified)

- **No ambient authority.** Only `host` imports; WASI fs/sockets/env/stdio trap
  (`wasm.rs:335` `define_unknown_imports_as_traps`), no `wasmtime-wasi`.
- **Narrow, copied model.** `get-node` is mirror-only and own-app-only (`instance.rs:252-264`);
  `fetch`/`text` return `denied`/`not-supported` (`instance.rs:269-278`); `TextChanged` carries no text.
- **Password names withheld** (`convert.rs:107-135`, `host.rs:548-570`; branch `subtree_facts`
  withholds name and text for `PasswordEntry`).
- **CPU limits are CPU** (fuel 3M/ms + thread-CPU backstop + 50x wall ceiling, `wasm.rs:79-187`);
  host-work meter on every import including the WASI shims (`wasi_pure.rs:9-12`; M5 r2 N-2 closed).
- **Bounded lifts and answers** (`HOSTCALL_LIFT_BYTES` 64 KiB, `convert.rs:12-19` caps).
- **Strikes and disable** per slot and per extension, re-attach does not reset (`host.rs:448-457`).
- **Keymap precedence** user > core > extension, gorca modifier mandatory, multi-press reserved.
- **Catalog namespaces.** Probe: an extension's reference with an argument that smuggles
  `\u{1e}victim:secret` resolves to the plain text `victim:secret` on the host side and stays plain
  through the Reader's second `resolve` (M12a V-1 class closed).
- **Native backend cannot be loaded from a directory** (`host.rs:166-170`).

---

## 4. Findings (ranked)

Severity is for the **store** setting (A2 to A4). Today's first-party-only deployment is not
exposed to F-1 to F-5 except through bugs.

### F-1 (High): no aggregate Reader budget for inline hooks; a slow add-on is never penalised

- Where: `host.rs:653-679` (`present` loops over all matching slots, each a fresh 2 ms call),
  `host.rs:680-711` (`policy`), `host.rs:713-727` (terminal), branch `web.rs` `describe` (2 ms, Reader).
  Limits: `gorca-ext-api/src/lib.rs:91-95` (2 / 20 / **16** ms); calibration `wasm.rs:102-105`
  ("typical hook code (1 to 3 operations per ns) near 2 to 6 ms").
- Measured earlier (M5 r2, `docs/reports/m5-rereview.md` M-1/N-2): a clock loop in `present` faults at
  2.2 to 4.8 ms; before the N-2 fix a self-timed `present` held the Reader 5.08 ms on every call
  with 0 faults in 300.
- Failure scenario: 5 store add-ons with an empty `app_name` matcher (matches everything,
  `docs/design/extensions.md` §5 "omitted = any") each implement `present` and return `None` after
  ~1.8 ms of work. Every focus change in every app pays 5 × 1.8 to 5.4 ms = 9 to 27 ms of Reader time
  before the core speaks: the 15 ms p99 is gone with no fault, no strike, no log. A terminal policy
  that spends 15 ms per `step` makes every key in the terminal 15 ms late, forever.
- Fix:
  1. A per-message **aggregate inline budget** owned by the Reader (for example 2 ms total for all
     extensions per focus/key), passed into `AppExts::present/policy/describe`; extensions after the
     budget are skipped ("no opinion") and charged a *slow* mark.
  2. **Cost accounting, not just faults**: per-(extension) EWMA and p99 of CPU per hook; an add-on
     over a soft budget (for example p99 > 0.5 ms inline) is demoted (moved off the inline path, or
     disabled with one spoken notice and a diagnostics entry).
  3. Third-party inline limit well below 2 ms (0.5 ms), fixed by tier; `terminal` and `web.describe`
     limits of 16/2 ms are first-party tier only.
  4. Per-extension hook cost is a store gate (section 8.4).

### F-2 (High): the `terminal` interface is a keylogger and full-screen reader, and the first matching add-on wins it

- Where: `gorca-core/src/terminal.rs:484-496` sends every key press as `TermInput::Raw` to the policy;
  `terminal.rs:676-681` applies the core's fail-closed `secret_hold` **after** `sess.step`, so raw keys
  typed at a password prompt still reach the extension. Policy selection: `host.rs:719-722` takes the
  first slot with the `TERMINAL` hook; order is specificity then load order, and load order is the
  sorted directory listing (`host.rs:139`, `manifest.rs:128-138` stable sort).
- Failure scenario: a store add-on "a-term-colors" with `app_name = ["gnome-terminal*"]` and hook
  `terminal` sorts before `ext-vte` and becomes the terminal policy. It receives every `raw-key`
  (keysym + unichar) including each character of `sudo`'s password, plus every screen (up to 256 KiB)
  via `screen(text, caret)`. It can stash them in KV today (`settings` capability) and send them out
  the day any network capability exists. The user hears nothing different if it forwards to
  `ext-vte`-equivalent logic.
- Fix: `terminal` (and any future raw-key or screen-content hook) is a **privileged capability**
  not grantable to store add-ons in the first store release (first-party or explicitly
  "trusted-signed" only). Independently of the store: while `secret_hold` is set, the core must
  replace typed characters with a masked key (`typed('*')` or `other`) *before* `step`; selection of
  the terminal policy must be explicit (one policy per app, chosen by the user or by a first-party
  pin), never by name sort order.

### F-3 (High): speech replacement, suppression and interruption need no capability and are not attributed

- Where: `capabilities` gate only `host.speak` (`instance.rs:280-297`), settings and fetch
  (`manifest_file.rs:141-149`). Unchecked paths: `present` `Some(..)` replaces the focus utterance
  (`host.rs:669-676`); `on-event` `rewrite`/`suppress` (`host.rs:614-631`; `app_actor.rs:1061-1077`
  speaks rewrites as `ExtSpeech`); branch `web.describe` replaces focus, quick-nav and line reading of
  claimed nodes, and `live` rules turn any selector into an **assertive** live region.
  Interrupt: `host.speak(.., prio=interrupt)` from any hook becomes `ExtSink::speak(text, true)`
  (`instance.rs:295`), and the Reader speaks it with `Prio::Interrupt` (`reader.rs:719-724` →
  `commands/handlers.rs:39-44`), cutting current speech. `ExtSink::speak` documents `interrupt` as "true
  for answers to a user command" (`gorca-core/src/ext.rs:120-122`) but nothing enforces it.
- Failure scenario (T-3/T-5): an add-on with no declared capability and an empty matcher returns
  `present = ["Cancel", button]` for a focused "Delete account" button in any app; or, with `speak`,
  calls `speak(.., interrupt)` from `on-event` 10 times a second (the bucket is per (extension, app),
  `instance.rs:170-173`, so 30 apps give 300 interruptions per second across the desktop). The user
  cannot tell core speech from add-on speech.
- Fix: capabilities `present:replace`, `events:suppress`, `speech:unsolicited`, `web:live` and
  `apps:*` (wildcard matcher) are explicit grants; the host forces `interrupt = false` outside
  command answers; a **global** per-extension speech budget across apps (not per slot); a user setting
  (default on for third-party add-ons) to mark add-on speech (short earcon or voice change) and a
  "what said that?" command that names the add-on behind the last utterance. Core-critical speech
  (security prompts, `alert` role, the reader's own notices) must not be suppressible or replaceable by
  third-party add-ons.

### F-4 (High, store): identity is a self-declared name; `verified_shell` is self-granted; user directory will override first-party

- Where: duplicates rejected, first loaded wins (`host.rs:108-117`); the KV namespace is the name
  (`instance.rs:299-311`, `kv.rs:48-69`); catalogs are registered under the name (`host.rs:183-189`);
  `verified_shell = true` is a plain matcher key (`manifest_file.rs:161-166`); M18 plans
  "user extension directory and precedence over system ones" (`docs/plan/phase-3.md:406`).
- Failure scenario: a store package (or any same-uid writer, A6) installs
  `~/.local/share/gorca/extensions/ext-gnome-shell/` with `verified_shell = true`. Under the M18
  precedence rule it replaces the first-party Shell policy, inherits its saved settings, and
  decides what is spoken for every Shell dialog and OSD (`policy` is consulted only for apps with a
  profile, i.e. the Shell, `reader.rs:540-562`). A store add-on can also take any unused name of a
  future first-party add-on and later receive its settings.
- Fix: identity = (publisher key fingerprint, add-on id); a **reserved `gorca.*` namespace** that only
  the system directory may provide (system wins for reserved ids, never the user directory); the Shell
  and other connector-verified targets become grants, not matchers; KV/catalog namespaces derived from
  the identity, not the display name.

### F-5 (Medium-High): shared instance pool and no global memory budget; pool exhaustion strikes the wrong add-on

- Where: `POOL_INSTANCES = 512` for all add-ons (`wasm.rs:106-107, 240-250`); 32 MiB per instance
  (`gorca-ext-api/src/lib.rs:97`) and a command may grow to the full cap (`wasm.rs:393-396`); an
  instance that cannot be built at attach (pool exhausted) is a strike against the add-on being
  attached (`docs/design/extensions.md` §3, "per extension").
- Failure scenario: 20 wildcard add-ons × 30 applications = 600 instances > 512: attaches start
  failing for whichever add-on comes next, e.g. `ext-vte` for a new terminal window; three such
  failures in 60 s disable `ext-vte` for every new terminal. Memory: 512 × 32 MiB = 16 GiB of
  potential RSS on a 30 GB host shared with the user's desktop.
- Fix: per-add-on quotas (max live instances, memory per add-on summed over apps, e.g. 64 MiB), lazy
  instantiation (attach only when a hook actually needs to run for that app), pool exhaustion is
  never a strike for the victim, first-party add-ons get a reserved pool slice, a global memory ceiling
  that evicts idle third-party instances first.

### F-6 (Medium): loader can hang startup forever or OOM on a crafted package (measured)

- Where: `host.rs:174-180`: `metadata(&mpath).len()` then `std::fs::read(&mpath)`, no regular-file
  check, no `O_NOFOLLOW`, no bounded read; the daemon awaits the whole load with no deadline
  (`gorca-daemon/src/wire.rs:280-301`). The catalog reader already does it right
  (`host.rs:764-788`, `read_catalog_file`), the module reader does not.
- Probe (same sequence): FIFO module → `metadata len = 0`, `read` blocks (killed by `timeout 3`,
  exit 124). Symlink to `/dev/zero` → `len = 0`, `read` returns `Err(OutOfMemory)` after 345 ms under
  `ulimit -v 2 GB`; without a ulimit it eats host memory.
- Failure scenario: a store archive that extracts a FIFO or device symlink as `x.wasm` leaves the user
  with no screen reader at the next login. Also: every start recompiles every component (no
  `Module::serialize`/cache in `gorca-ext-host`); with dozens of store add-ons, startup grows with no
  bound.
- Fix: open with `O_NOFOLLOW | O_NONBLOCK`, `fstat` regular file, bounded `take(MAX_MODULE_BYTES+1)`
  read (reuse `read_catalog_file`), a deadline on the whole load with per-add-on skip, the installer
  rejects non-regular entries, and a precompiled-artifact cache keyed by (component hash, wasmtime
  version, engine config) verified before use.

### F-7 (Medium): `wasi:random/random` is not cryptographically secure

- Where: `wasi_pure.rs:6-7, 41-51, 70-94`: `get-random-bytes`/`get-random-u64` of `wasi:random/random`
  are a splitmix64 stream seeded from the wall clock and a global counter shared by all add-ons.
- Why it matters: the WASI contract of `wasi:random/random` is a CSPRNG; `getrandom`, `rand`,
  `uuid`, TLS and crypto crates trust it. The design doc's "do not rely on random numbers being
  unpredictable" will not reach authors of dependencies. Once network or auth capabilities exist,
  nonces and keys generated by add-ons are predictable.
- Fix: serve `wasi:random/random` from the OS (`getrandom(2)` into a per-store ChaCha buffer, still
  metered), keep splitmix only for `wasi:random/insecure`.

### F-8 (Medium): API compatibility is all-or-nothing

- Where: one world exports every interface (`wit/gorca-ext.wit:255-259` on main: `hooks`, `terminal`;
  branch adds `web` and bumps the package to `0.3.0`); every add-on must implement all exports, so each
  new interface is a breaking change for every add-on. `define_unknown_imports_as_traps`
  (`wasm.rs:335`) turns a mismatched import into a runtime trap (a strike, then disable) instead of a
  load-time rejection.
- Failure scenario: store add-ons built against 0.9 stop loading (or start trapping and get disabled
  with a spoken notice each) after a gorca update that adds an interface.
- Fix: several small worlds (`policy`, `presenter`, `terminal`, `site`, `commands`), each add-on
  targets a world; the host links several package versions side by side (adapters for N-1); the
  manifest declares `api = "0.9"` and the host refuses unknown/too-old versions at load with a clear
  diagnostic; unknown imports are a load error for third-party add-ons (traps only for the known
  WASI std-pulls).

### F-9 (Medium): settings KV: one shared plaintext file, rewritten whole, default permissions

- Where: `kv.rs:102-130` serialises all namespaces into one TOML and writes it with `std::fs::write`
  (mode `0666 & ~umask`, typically `0644`) + rename, no fsync; the worker flushes after every job or
  every second when dirty (`host.rs:274-279`, `KV_POLL` 1 s); quota 128 × 4 KiB per add-on.
- Failure scenario: an add-on that sets a changing value on every event triggers one rewrite of the
  whole file (up to 64 × 512 KiB = 32 MiB) per second on the same two workers that run commands and
  rebuilds. Future secrets (API tokens) land in a world-readable file if the home directory is
  `0755`; a crash during rename-without-fsync can lose every add-on's settings.
- Fix: per-add-on file under the state dir, mode `0600`, rate-limited and coalesced flush on its own
  thread, fsync before rename, a separate `secret-store` capability backed by libsecret (or refused).

### F-10 (Medium): quarantine is in-memory only and rate-based

- Where: `health.rs` (3 resets in 60 s, `gorca-ext-api/src/lib.rs:99-100`); state lives in the process.
- Failure scenario: an add-on faulting twice per minute per app is never disabled and costs up to
  its fuel limit inline each time; an add-on that crashes gorca's speech every session (after a restart
  all strikes are clean) is re-enabled on every login.
- Fix: a persisted health ledger per (add-on id, version): exponential backoff, "disabled after N
  sessions" quarantine, auto re-enable only on a new version, a spoken one-time notice and a
  diagnostics page. Count slow calls (F-1) as half-strikes.

### F-11 (Low): add-on log lines are injectable and may carry user data

- Where: `instance.rs:314-326` forwards `msg` (512 chars) to `tracing` at the add-on's chosen level,
  including newlines and control characters.
- Failure scenario: an add-on writes fake `gorca_core` log lines into a bug report, or logs screen
  text that the user then attaches publicly.
- Fix: escape control characters, prefix with the add-on id, cap level at `info` for third party,
  disabled by default outside a developer mode.

### F-12 (Low): secrecy is role-only

- Where: `convert.rs:110`, `host.rs:549`, branch `subtree_facts`: only `Role::PasswordEntry` is
  withheld. A password shown via "show password" (role becomes text entry), OTP and card-number
  fields, and secrets printed in a terminal are not marked.
- Fix: when text values reach the WIT (`TBD: text value` in `node`), the host must attach a
  sensitivity bit computed in the core (role, `autocomplete=one-time-code/cc-*`, terminal secret
  hold, the 2026-10-13 password decision's redaction rule) and withhold sensitive content by default.

### F-13 (Info, branch): site add-ons see the URL and node text

- `web.open` gets the URL (only for matching `document_url` globs, good), clipped to 2048 chars but
  with query and fragment (OAuth tokens, session ids); a glob `*` is the full browsing history.
  `describe` gets names and text of claimed subtrees (message contents).
- Fix: origin-only URL by default (`url:full` grant), store rejects over-broad globs, `describe` input
  limited to the claimed subtree (as now) and redacted per F-12.

---

## 5. NVDA comparison: what we must do better

| Aspect | NVDA (`refs/nvda/source/addonStore`, `addonAPIVersion.py`) | gorca target |
|---|---|---|
| Isolation | In-process Python, full user privileges, can patch NVDA itself | WASM components, no ambient authority (have it) |
| Permissions | None: install = full trust | Deny-by-default capabilities with spoken consent (section 6) |
| Integrity | sha256 from store metadata over HTTPS (`network.py:391-395`); no publisher signature; `baseServerURL` configurable | Publisher signatures + store countersignature + transparency log (section 8.5) |
| Review | Community submission to the datastore, automated checks, VirusTotal results shown (`models/addon.py:24,134`) | Automated gates that are meaningful for WASM: world conformance, capability diff, fuel/latency benchmark, hostile harness (section 8.4) |
| Compatibility | `minimumNVDAVersion`/`lastTestedVersion`, yearly `BACK_COMPAT_TO` break (`addonAPIVersion.py:17-23`) disables most add-ons once a year | Versioned worlds with adapters, additive minors, a deprecation window measured in releases, not a yearly cliff |
| Channels | stable/beta/dev/external (`models/channel.py:14-17`) | Same channels; "external" (sideloaded) is visibly marked and gets no privileged grants |
| Failure handling | An exception in an add-on can freeze NVDA (same thread) | Fault = reset, quarantine persisted, never a frozen reader |

NVDA's lesson: its ecosystem is rich because add-ons can do anything; its weakness is the same fact.
gorca must give add-ons enough reach (sections 6, 8.1) that authors do not demand native escape
hatches, while keeping every dangerous power behind a grant.

---

## 6. Required capability model (deny by default)

Granted per (add-on id, version); an update that adds a capability needs fresh consent (section 8.5).

| Capability | Grants | Default | Store tier |
|---|---|---|---|
| `apps:<globs>` | Attach to the named apps; wildcard or > N apps is its own grant `apps:*` | Named apps only | All; `apps:*` reviewed |
| `shell` | Attach to the connector-verified GNOME Shell | Off | Trusted only |
| `present:replace` | `present`/`describe` may replace core speech for matched apps | Off | Reviewed |
| `events:suppress` | `on-event` may suppress or rewrite | Off | Reviewed |
| `speech` | `host.speak` as a notice (never interrupt) | Off | All |
| `speech:interrupt` | Interrupt outside command answers | Never for third party | — |
| `keys:commands` | Declared commands with gorca-modifier chords (have it) | On if declared | All |
| `keys:raw` / `terminal` | Raw key stream, terminal screen | Off | First-party / trusted-signed only |
| `text:read` (future) | Node text values, sensitivity-redacted | Off | Reviewed |
| `text:sensitive` | Unredacted sensitive text | Never | — |
| `web:<origins>` | Site add-on for listed origins; `url:full` separately | Off | Reviewed per origin |
| `web:live` | Turn nodes into live regions | Off | Reviewed |
| `settings` | KV, quota | On if declared | All |
| `clipboard:read` / `clipboard:write` | Read is high risk (passwords copied from managers) | Off | Read: trusted only |
| `network:<hosts>` | Outbound to listed hosts only, via a host-side fetch import with deadlines, size caps and per-add-on rate; no sockets | Off | Reviewed; never combined with `keys:raw`/`terminal`/`text:sensitive` in one add-on |
| `notify` | Desktop notifications | Off | All |

Hard rules: (a) **no add-on may hold both a high-sensitivity input** (`keys:raw`, `terminal`,
`clipboard:read`, `text:read` on apps:*) **and `network`**; the host refuses the combination even if
the manifest asks. (b) Capabilities are enforced in the host at the import or at the call site, never
by the guest. (c) The core's own security-relevant speech (alerts, auth dialogs, its notices) cannot be
suppressed or replaced by third-party add-ons.

### 6.1 Consent UX for a blind user

- **Spoken, short, in the reader's own voice and mode** (in-reader `Mode::Prompt`, M15a), never a
  visual dialog. Example: "Telegram helper wants to: read Telegram Web pages, change what is spoken
  there. Allow? Y for yes, N for no, D for details."
- Capabilities are grouped into four plain-language classes: *read what's on screen in …*, *change
  what gorca says in …*, *read your typing*, *connect to the internet (sites)*. "Read your typing" and
  "internet" are always spoken separately and require a second confirmation.
- Default answer is **no** on timeout or Escape; a grant is never inferred.
- Update with new capabilities: the add-on keeps running with its old grants until the user answers
  the diff prompt ("Telegram helper update also wants: connect to telegram.org").
- A review command: "list add-ons and permissions" in the chooser (M15), revoke with one key.
- Attribution: "what said that?" names the add-on behind the last utterance (F-3).

### 6.2 Auditing and diagnostics (telemetry-free)

- Local per-add-on ledger: capabilities granted (when, which version), hook CPU p50/p99, faults,
  disables, speech count, suppress/replace count, KV bytes, network bytes per host. Kept in the state
  dir, rotated, never uploaded.
- `gorca diag addons` command and a spoken summary; an export for bug reports that redacts text and
  includes only counters and versions.
- Network capability calls are logged with host and byte counts (never payloads).

---

## 7. Stability requirements

### 7.1 API compatibility policy (feeds ADR-0015)

- Semver per world. Within `0.x` preview: additive changes only within a minor; a breaking change gets
  a new minor and the host keeps the previous minor's linker for at least two gorca releases.
- From 1.0: additive within major; host links N and N-1 majors via adapters; deprecation announced one
  release ahead in the diagnostics ledger and the store listing.
- Optional exports: the host probes which interfaces a component exports; missing ones mean "not
  provided" (F-8).
- Every WIT change runs a compatibility check in CI (component built against the previous version must
  still link and pass the conformance suite).

### 7.2 Crash, restart, quarantine

- Keep: fault = reset from `InstancePre`, per-slot and per-add-on strikes.
- Add: persisted ledger with backoff (F-10), "slow" demotion (F-1), session-level quarantine
  ("disabled after crashing in 3 sessions"), safe mode (`gorca --no-addons`, also reachable by a key
  chord at startup) and automatic safe mode if the reader restarts more than K times in M minutes with
  third-party add-ons loaded.

### 7.3 Per-add-on quotas

| Resource | First-party | Third-party (proposed) |
|---|---|---|
| Inline hook CPU | 2 ms (terminal 16) | 0.5 ms, plus the aggregate inline budget (F-1) |
| Command CPU | 20 ms | 20 ms, one worker share (fair queue per add-on) |
| Memory | 32 MiB per instance | 16 MiB per instance, 64 MiB per add-on summed |
| Instances | reserved pool slice | ≤ 16 live, lazy attach |
| Speech | 10/s per slot | 10/s per add-on **globally** |
| KV | 128 × 4 KiB | 64 × 1 KiB, flush ≤ 1/10 s |
| Network (future) | n/a | per host allow-list, ≤ N req/min, ≤ M KiB/min, 2 s deadline |
| Startup | compile cached | compile cached, load deadline per add-on |

### 7.4 Testing kit for add-on authors

- `gorca-ext-sdk` ships: a native test harness (`Hooks` against a fake host, already possible), a
  component harness (the `tests/common` host fixture as a library), **the hostile suite as a
  conformance check run against the author's component** (fuel, host-work, memory, lift caps, budget
  compliance), and a latency report (per hook p50/p99 CPU on a reference trace).
- Recorded traces (AT-SPI event streams for Firefox, Terminal, Shell) to replay against the add-on.
- `cargo xtask ext-check <component>`: world conformance, declared vs used imports, capability lint,
  manifest lint (globs, chords), size, cold-compile time.

### 7.5 Store review gates (automated, every upload)

1. **Structure**: component parses; imports ⊆ the declared world(s); no unknown imports; exports
   match the manifest hooks; size ≤ limit; only regular files in the archive.
2. **Capability diff** vs previous version: any new capability or broadened matcher/origin forces
   human review and user re-consent.
3. **Forbidden combinations** (section 6 rule a).
4. **Performance**: hook CPU on reference traces under the third-party budget (p99), memory peak,
   no faults; compile time bound.
5. **Hostile behaviour scan**: replay the trace and flag unsolicited speech, suppression of alert/
   dialog events, `present` returning different text than the node name for control roles (spoof
   heuristic), KV write rate.
6. **Reproducible build** (recommended, required for `trusted` tier): source + lockfile + toolchain
   rebuild yields the same component hash.
7. Human review for: `present:replace`, `events:suppress`, `apps:*`, `web:*`, `network`.

### 7.6 Signing and key management

- **Publisher keys** (Ed25519) sign the package (manifest + component + catalogs, canonical archive
  hash). The store **countersigns** after review. gorca verifies both before install and on every load
  (hash pinned in the install record).
- **Store root key** offline (HSM or air-gapped), signs short-lived online signing keys; the client
  ships the root key and a pinned rotation policy (TUF-style: root, targets, snapshot, timestamp
  roles) so a CDN compromise (A4) cannot serve old or altered packages (freeze/rollback protection).
- **Transparency log** of every published (id, version, hash, capabilities); clients check inclusion.
- **Key loss/compromise**: publisher key rotation signed by the old key or by the store after identity
  recheck; revocation list fetched with the index; a revoked add-on is disabled with a spoken notice.
- **Sideloading** stays possible ("external" channel) with a spoken warning, no privileged tiers, and
  a visible marker in diagnostics.
- The reader itself never downloads or verifies in the Reader/hot path: install and update run in a
  separate process (or the settings app), with deadlines; the daemon loads only verified, installed,
  precompiled artifacts.

---

## 8. Prioritised work list

Before M18 (SDK preview, third-party add-ons from a user directory):

1. F-6 loader hardening + load deadline + compile cache.
2. F-4 identity: reserved first-party namespace, system wins for reserved ids, `shell` as a grant;
   change the M18 "user precedence over system" rule accordingly.
3. F-1 aggregate inline budget + per-add-on cost accounting; third-party inline limit.
4. F-2 core masks keys under `secret_hold` before `terminal.step`; `terminal` first-party only;
   explicit terminal policy selection.
5. F-3 capabilities for replace/suppress/interrupt, host-enforced `interrupt = false`, global speech
   budget, attribution command.
6. F-8 multiple worlds + manifest `api` version + load-time rejection.
7. F-7 CSPRNG for `wasi:random/random`.

Before the store (Phase 4):

8. F-5 quotas and pool isolation; F-9 KV per add-on, 0600, rate-limited; F-10 persisted health ledger.
9. Consent UX (6.1), diagnostics ledger (6.2), SDK testing kit (7.4).
10. Store gates (7.5), signing/TUF/transparency (7.6), network capability design with the forbidden
    combinations.

## 9. Open questions / unverified

- F-1 aggregate cost is computed from the code's own fuel calibration and the M5 r2 measurements, not
  re-measured with N concurrent add-ons on this host (host load 17 to 21 during the review).
- F-5 strike-on-pool-exhaustion is taken from `docs/design/extensions.md` §3; not reproduced with 512
  live instances.
- F-8: the exact wasmtime 49 behaviour when a 0.2.0 component meets the 0.3.0 world (missing export
  error vs import traps) was not run; the branch rebuilt every in-tree add-on.
- Whether `web.describe` input can carry an `<input>`'s typed value (the `text` of a text node) was
  not traced through the branch's mirror.
