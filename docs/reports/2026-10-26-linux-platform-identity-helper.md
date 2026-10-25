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

One line each way on a stream socket: `ID <pid> <start-ticks>\n` -> `OK <identity>\n` | `NO\n`. The helper
checks the start time before and after its reads. The client never turns a missing helper into a weaker
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
