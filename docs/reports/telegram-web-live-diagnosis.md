# Telegram Web plugin: why the owner heard no difference (2026-10-25)

## Verdict

The plugin never ran for the Firefox document. `plugin scope opened` (INFO, `gorca_daemon::plugins`)
is absent from the whole journal of the unit (0 hits), although `plugin loaded` and
`plugin runtime ready` are there.

## Root cause 1 (activation): the systemd unit's namespaces make every native app "unverified"

* `Entry::matches` (crates/gorca-daemon/src/plugins.rs) lets a URL-scoped plugin open only for a
  verified peer identity (`scope.verified`).
* The identity comes from `gorca-connector-atspi/src/proc.rs::verified_identity`. It required the peer to
  be in gorca's own mount namespace (`same_ns`), else it asks for flatpak evidence and returns `None`.
* The unit (`~/.config/systemd/user/gorca.service`) has `PrivateTmp=yes`, `ProtectSystem=strict`,
  `RestrictNamespaces=yes` and so on: systemd gives gorca its own mount AND user namespace.
  Measured: gorca (pid 689561) `mnt:[4026533185]`, `user:[4026533184]`; Firefox (pid 4531, /usr/lib/firefox-esr/firefox-esr,
  root-owned) `mnt:[4026531832]`, `user:[4026531837]` (the initial one).
* So `firefox-esr` was never verified; `scope_identity` gave `(self-reported name, false)`; the URL-scoped
  scope stayed closed. Every other native app was unverified as well (not only Firefox).
* Not the cause (checked): URL glob (`DocURL` = `https://web.telegram.org/a/#728451375` matches
  `https://web.telegram.org/a/*`, fragment ignored), the app list (`firefox-esr` is in it), the trust tier
  (FirstParty, pin matched), `report_url` wiring, the `exe` trust rules (`/usr`, `/usr/lib`, `/usr/lib/firefox-esr` all root 755).
* A first fix attempt compared with the launcher's (systemd --user) namespace; it fails live:
  `/proc/<systemd --user>/ns/mnt` is unreadable to the user (the manager has a capability, so
  it is not ptrace-readable). Found by reading it before shipping.

### Fix

`identity_in`: a peer in a foreign mount namespace is accepted as native when it is in the **initial
user namespace** (an unprivileged process gets a private mount namespace only through a user namespace
of its own, so such a peer was placed there by privileged software), no `.flatpak-info` is there (flatpak
keeps its evidence path, unverified), and its `/proc/<pid>/exe` is the very same file (device and
inode) that gorca sees at the canonical root-owned path (a bind-mounted look-alike fails this). An
`unshare -Ur` impostor stays unverified (existing test) and an unreadable user namespace fails closed.
Live check (read-only): `GORCA_PROBE_PID=4531 GORCA_PROBE_OURS='mnt:[4026533185]'` with the ignored
test `live_peer_identity_probe` gives `Some("firefox-esr")` (before: `None`).

## Cause 2 (matching): Telegram Web A changed

Live tree (Firefox ESR, read-only AT-SPI, 2026-10-25), chat row:
`link [class=ListItem-button]` with children `Avatar`, `button.fullName`, `section.time`,
`section.last-message > sender-name, colon, last-message-summary`, and the unread counter in a block with
only hashed classes (`NT-X2xr9 yf73Yld8 npoDLolY`, text `529`). The `[[watch]]` classes still match
(`ListItem-button`, `Message` on `div.Message`), so name and article facts would be produced. Two gaps:

* the counter was never recognised (the plugin looked for `ChatBadge`), so "unread" would not be said
  even with the plugin active; fixed by a positional rule (a bare count, `529`, `182,6K`, right under
  the row, outside any title/time/preview part; `looks_like_count`);
* the preview read `Алексей : text` (separate leaves); a lone `:` now sticks to the word before it.

Native name today is `Q148 Q148 22:28 Алексей : ...` (title doubled), so any applied fact is audible.

## Candidate 3 (effect on speech)

Not reproduced end to end (no private Firefox session run). The daemon tests drive the real wasm through
the bridge and show the fact; refinement vs first utterance behaviour was not changed. After activation,
the first focus on a row that was not derived yet waits at most the labels-mode wait budget
(`a_focus_on_an_underived_row_waits_at_most_the_deadline_and_the_facts_follow`).

## What the owner must do

Rebuild/redeploy gorca (the identity fix is in the binary) and install the rebuilt plugin
(`plugins/dist/gorca.telegram-web`, new pin in `plugins/pins.txt`: the old installed wasm no longer matches
the new binary's pin, so install both together). No config change. After restart, the journal should show
`plugin scope opened app=firefox-esr doc=true plugins=1` once the Telegram tab's document is attached
(INFO, no extra env needed). For facts-level logging (`plugin fact`, `plugin log`, grants) set
`RUST_LOG=info,gorca_daemon::plugins=debug` in a service drop-in (not done here).

## Side effect to watch

All native apps now get a verified identity (e.g. `gnome-terminal-server`, `firefox-esr`), which the
connector and the profiles use for verified-name behaviour that was effectively off in the service.
Unit tests pass, but this has not been exercised live for other apps.

## Evidence

* journalctl --user -u gorca: 0 `plugin scope opened`; `readlink /proc/{689561,4531}/ns/{mnt,user}` above.
* Tests: `cargo test -p gorca-connector-atspi --lib proc::` (14 pass), `-p gorca-daemon --test plugin_runtime` (11 pass,
  incl. the live-shaped row), plugin crate 10 pass; clippy `-D warnings` on both crates; xtask deps/strings/legacy ok.

## Follow-up (22:49 restart, still no `plugin scope opened`): the real root cause

The first fix was unreachable. Measured 2026-10-25 23:2x with `systemd-run --user --pipe` transient units:

| unit properties | `readlink /proc/4531/exe` (Firefox) | own user ns |
|---|---|---|
| `NoNewPrivileges RestrictSUIDSGID RestrictNamespaces LockPersonality RestrictAddressFamilies` | ok | initial |
| `+ PrivateTmp=yes` (or `ProtectSystem=strict`, `ProtectKernelTunables`, `ProtectKernelModules`, `ProtectControlGroups`, each alone) | **Permission denied** | private |

Any mount-namespace sandboxing in a *user* unit creates a user namespace (gorca pid 729030:
`user:[4026533184]`, `uid_map` `1000 1000 1`). From there the kernel refuses every ptrace-class
`/proc/<pid>/` entry of processes outside it: `exe`, `ns/*`, `root`, `cwd`, `environ`, `maps`
(`stat`, `status`, `cmdline`, `cgroup`, `mountinfo` stay readable). So `verified_identity` gets
no exe and no namespace for any peer: unverified, URL-scoped scope never opens. (Root-owned files
also show owner 65534 there; moot, since the exe read fails first.) The earlier namespace rule only
helps a process that can read the peer, which a private-user-ns service cannot.

Probe in the transient units (`live_peer_identity_probe`, whole real path): hardened unit
`private user ns: true, identity: None`; same unit without the namespace options
`private user ns: false, identity: Some("firefox-esr")`.

### Fix and what to do
* `contrib/systemd/gorca.service`: drop `ProtectSystem`, `ProtectKernelTunables`,
  `ProtectKernelModules`, `ProtectControlGroups`, `PrivateTmp` (keep NoNewPrivileges,
  RestrictSUIDSGID, RestrictNamespaces, LockPersonality, RestrictAddressFamilies,
  SystemCallArchitectures).
* Owner: apply the same to `~/.config/systemd/user/gorca.service`
  (`systemctl --user daemon-reload`, restart). Drop-in alternative cannot remove options;
  reset them with an override file that sets `ProtectSystem=no`, `ProtectKernelTunables=no`,
  `ProtectKernelModules=no`, `ProtectControlGroups=no`, `PrivateTmp=no`.
* Daemon logs a WARN at startup when plugins are enabled and gorca is in a private user namespace.
* Expected after restart: `plugin scope opened app=firefox-esr doc=true plugins=1`.

## Update 2026-10-26

The unit-side workaround (dropping the namespace options) is superseded: the full sandbox is back and identity comes from the identity helper, see `docs/reports/2026-10-26-linux-platform-identity-helper.md`.
