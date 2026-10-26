# Application identity inside the full systemd sandbox (2026-10-26)

Problem: URL-scoped plugins open only for a verified application identity (ADR-0026 section 2.7).
Identity needs `/proc/<pid>/exe`. A systemd *user* unit with `ProtectSystem=`, `PrivateTmp=`,
`ProtectKernelTunables=`, `ProtectKernelModules=`, `ProtectControlGroups=` (also `ProtectClock=`,
`ProtectHostname=`, `ProtectKernelLogs=`, `PrivateUsers=`, `ReadOnlyPaths=`, `ProtectHome=`) puts gorca in a
private user namespace, and the kernel then refuses `exe`, `cwd`, `root`, `ns/*`, `environ`, `maps`, `io`,
`smaps_rollup` of processes outside it. Commit 1aee82fa shipped a unit without those options. The owner
rejects weakening the sandbox. This change restores them and moves the privileged read out of the sandbox.

## What is readable from inside the namespace (measured, Firefox ESR pid 4531, kernel 7.1.8-rt, systemd 257)

`systemd-run --user --pipe --wait` with `NoNewPrivileges ProtectSystem=strict ProtectKernelTunables
ProtectKernelModules ProtectControlGroups PrivateTmp RestrictNamespaces`:

| Source | Plain | Inside the userns | Self-chosen by the app? |
|---|---|---|---|
| `/proc/<pid>/exe`, `cwd`, `root`, `ns/{mnt,user,pid}` | yes | **denied (empty)** | no (kernel) |
| `environ`, `maps`, `io`, `smaps_rollup` | yes | denied (EACCES) | - |
| `stat`, `status`, `comm`, `cmdline`, `limits` | yes | yes | `comm`, `cmdline`: yes |
| `cgroup` (`app-gnome-firefox\x2desr-N.scope`) | yes | yes | not by a plain app; same-user can `systemd-run --scope --unit=...` |
| `mountinfo`, `attr/apparmor/current` | yes | yes | no (kernel label) |
| D-Bus `GetConnectionCredentials` (dbus-broker, session bus) | yes | yes: `UnixUserID`, `ProcessID`, `LinuxSecurityLabel`, `ProcessFD` (pidfd) | no (broker) |

Per-option test (one `-p` at a time): `NoNewPrivileges`, `RestrictNamespaces`, `LockPersonality`,
`RestrictSUIDSGID`, `ProtectProc=invisible`, `ProcSubset=pid`, `SystemCallFilter=@system-service` keep the
initial user namespace (exe readable). `CapabilityBoundingSet=` (empty) fails in a user unit with
`218/CAPABILITIES`. Every `Protect*`/`Private*`/`ReadOnlyPaths` option creates the namespace.

## Options evaluated

1. **In-sandbox only (cgroup scope + desktop id + cmdline + broker credentials).** Gives the launcher-chosen
   unit and the pidfd (kills pid reuse), but no executable evidence: a binary copied to `$HOME` and started
   with `systemd-run --scope --unit=app-gnome-firefox-1` would pass. Threat model lower than today (cheap
   spoofing of ADR section 2.7 item 2). Rejected as the only source; remains a cross-check (flatpak already
   uses the scope).
2. **Keep only non-namespace hardening.** Possible set listed above; it is what 1aee82fa shipped. Rejected
   by the owner (loses read-only `/usr`, private `/tmp`, kernel tunables).
3. **Identity helper outside the sandbox (chosen).** `gorca --identity-helper`, own unit
   `gorca-identity.service` with every hardening option that creates no namespace (NoNewPrivileges,
   RestrictNamespaces, RestrictSUIDSGID, LockPersonality, MemoryDenyWriteExecute, RestrictRealtime,
   RestrictAddressFamilies=AF_UNIX, SystemCallFilter, UMask=0077). Unix socket
   `$XDG_RUNTIME_DIR/gorca/identity.sock` (dir 0700, socket 0600). It runs the **same** `verified_identity`
   as before (exe inode, root-owned path, mount/user namespace rule, snap label, flatpak evidence), so the
   decision logic and threat model are unchanged. gorca (any user namespace) connects: connect to a socket
   works under `ProtectSystem=strict`.
4. **AT-SPI self-reported info** (toolkit, app name, bus name): kept only as the display name, never as
   evidence (unchanged, ADR section 2.7).

Why not socket activation: `OwnedFd::from_raw_fd` is unsafe; the helper binds its own socket and
`gorca.service` has `Wants=`/`After=` on it. A look-alike server squatting on the path while the helper is
down is refused by an `SO_PEERCRED` pid -> `/proc/<pid>/cgroup` check on both ends (helper serves unit
`gorca.service` only, client trusts unit `gorca-identity.service` only). A same-user process that moves
itself into those cgroups is the same-user adversary the ADR does not cover.

## Protocol

Tagged lines on a stream socket, pipelined, replies in any order: `ID <tag> <pid> <start-ticks>\n` ->
`OK <tag> <identity>\n` | `NO <tag>\n` (a non-request line gets a bare `NO\n`). The helper checks the
start time before and after its reads. The client never turns a missing helper into a weaker
in-sandbox guess: no answer is unverified (`None`) and one `warn` is logged. Where gorca is **not** in a
private user namespace (plain unit, tests, a terminal run) the client reads `/proc` itself as before; no
IPC.

## Evidence (transient units, real Firefox pid 4531, read-only)

Helper: `systemd-run --user --unit=gorca-idtest-helper` with the helper hardening (initial userns,
`NoNewPrivs 1`, `Seccomp 2`, socket `srw-------` in a `drwx------` dir). Client: the ignored test
`identity_svc::tests::live_helper_identity_probe` inside a transient unit with the **full** sandbox of
`contrib/systemd/gorca.service` (`private-user-ns=true`).

| Case | Result |
|---|---|
| Firefox ESR (`/usr/lib/firefox-esr/firefox-esr`) via helper, full sandbox | `Some("firefox-esr")` |
| Copy of `sleep` named `firefox-esr` in the worktree (user-writable) | `None` |
| `unshare -Urm sleep` impostor | `None` |
| Client expects the wrong server unit | `None` (Untrusted) |
| Client runs in a unit the helper does not serve | `None` (connection dropped) |
| Helper stopped | `None` (Unreachable), one warning |
| Latency, debug build, helper round trip incl. connect | first 2.4-10 ms (once per application, off the key path, `spawn_blocking` under the existing 250 ms deadline), mean 1.9 ms over 100 |
| Unit tests (`cargo test -p gorca-connector-atspi --lib`, `-p gorca-dbus`) | 213 + 33 pass; 8 new protocol/helper tests, 1 `peer_pid` test |

Not testable here: a real flatpak or snap process (neither installed). Their evidence parsers keep their unit
tests, and the helper uses the unchanged `identity_in`; `/proc/<pid>/root/.flatpak-info` is read from the
initial namespace as before, which is where it already worked.

## Residual risks

- A second process (about 3 MB RSS) and a second unit to deploy; `gorca.service` only `Wants=` it, so gorca
  still starts without it (identities unverified, one warning).
- The helper is unsandboxed with respect to mounts: it reads `/proc` of same-user processes and answers any
  `gorca.service` peer. Its attack surface is a 64-byte line parser; filtered syscalls, no network, no caps
  to lose. A compromised gorca can ask it about pids (it learns names of root-owned executables only).
- Same-user cgroup moves defeat both peer checks (out of scope, ADR section 2.7).
- `CapabilityBoundingSet=` could not be used in a user unit (218/CAPABILITIES).
- Stale socket file remains after the helper stops; the next start replaces it, a client sees ECONNREFUSED.

## Review round (IH-1..IH-3) and scaling requirement (2026-10-26)

Design after review. The first version capped the helper at 8 connections and dropped the 9th; the owner's
requirement is that the number of applications is unbounded and nothing is refused.

* **One persistent connection, tagged pipelining** (`identity_client::Client`). A reader thread routes
  replies to the callers waiting on them; a caller never blocks others. The helper reads each connection on
  its own thread and queues requests (unbounded queue, memory-bounded) for 8 worker threads; replies are
  tagged and unordered. Connections: up to 256 at once, a further one waits in `accept` (never dropped).
* **Cache and de-duplication.** `(pid, start)` -> identity (verified or `None`), concurrent lookups of one
  key share one wire request, 50 windows of one process cost one lookup. `forget(pid, start)` runs when the
  application actor ends (pid reuse is also covered by the start time in the key). Failed lookups (helper
  absent, timeout) are never cached. Bounded at 8192 finished entries.
* **IH-1** no drop under load (above). **IH-2** `connect` has a deadline: `gorca_dbus::connect_unix_timeout`
  (non-blocking connect, `EAGAIN` retried until the deadline), so a frozen helper no longer leaks a blocking
  thread. **IH-3** transient errors (helper not bound yet, dropped exchange) are retried within the 200 ms
  budget; the helper unit is `Type=notify` and sends `READY=1` after `bind`, so `gorca.service` (After=)
  starts when the socket exists; and an application actor whose identity stayed unverified re-asks with
  backoff 0.5/1/2/5/15 s (only when the helper is in use) and re-announces on success.
* **Deployment path.** The owner's installed units are copies pointing at a release binary, so both units
  must name the same binary: `GORCA_BIN=/path/to/gorca tools/install-dev.sh` writes both copies with
  `ExecStart` rewritten (without it the units are symlinked and use `%h/.local/bin/gorca`).

Measured (debug build, helper in the initial namespaces, client in a transient unit with the full sandbox):

| Case | Result |
|---|---|
| 200 processes outside the sandbox, 64 caller threads, one connection | all 200 verified in 18-25 ms (unit test and `systemd-run` proof) |
| the same 200 again (cache) | 80 microseconds in total |
| Firefox via helper, full sandbox | `Some("firefox-esr")`, first 2.5 ms |
| 50 concurrent lookups of one process | one answer, all equal |
| Helper bound 30 ms late / backlog full / 9+ held connections (audit tests) | answered / returns at its deadline / answered |
| `unshare` impostor, `$HOME` copy, wrong server unit, wrong client unit, helper down | `None` (as before) |

Live probes (ignored tests) read their parameters (`GORCA_PROBE_PID`, `GORCA_PROBE_PIDS`, `XDG_RUNTIME_DIR`, `GORCA_IDENTITY_*`) as `KEY=value` lines from `/var/tmp/gorca-identity-probe.params`; only the daemon main.rs reads the environment (xtask rule 8).
