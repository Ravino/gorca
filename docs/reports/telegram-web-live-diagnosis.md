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

## Follow-up 2 (scope opens, but `Malformed` / `Watchdog` faults)

Reproduced with a live dump of the Telegram A document (`live_tree_probe`, ignored test, input
`GORCA_LIVE_TREE=<json>`; 404 nodes, 30 chat rows): one background job with all rows ended in
`Watchdog` (debug host; the owner's release build shows it once and `Malformed` mostly).

* Cause 1 (`Watchdog`, and the strikes that mask the plugin): `PluginBridge::background` sent up
  to `MAX_BATCH` = 64 dirty rows in one call. A Web A row costs a `subtree` of up to 64 nodes plus
  one `attr` host call per node; 64 of them exceed the 1 ms background grant, and the host kills a
  call at 4 grants of CPU.
* Cause 2 (`Malformed`): a call may return at most 32 KiB of fact text and 1 KiB per message
  argument (B-6); 64 rows with Cyrillic previews (2 bytes a letter, up to 348 characters) cross
  either limit, and one such row fails the whole call, so every other row of its batch loses its name.
* Cause 3 (host bug found on the way): `owned_by` attributed every effect of a multi-target call to
  every target (the read set is the union), so a batch of k rows admitted k^2 facts
  (10 rows gave 100 ops, each name 10 times) and kept k^2 tokens.
* Not related: `RLIMIT_RTTIME SIGXCPU` on `gorca-input` (5 earlier occurrences in the journal since
  2026-10-23, all before plugins were enabled; the input lane's pre-wake only arms, never spins).

Fix: the bridge sizes background jobs per plugin from the measured microseconds per node
(`BG_JOB_TARGET_US` = 250 per job, 1..32 nodes, 4 before the first measurement); a faulted call halves
the size and its nodes are requeued; the plugin clips title/preview/count/time arguments (160/240/12/24
characters); facts are owned by their own target. New pin in `plugins/pins.txt`.
Tests: `a_long_busy_chat_list_derives_without_faults_and_without_duplicate_facts` (90 long rows, failed
before: 2 jobs, 0 admitted), the live dump now gives 30 facts, 1 per row, no fault; `long_parts_are_cut...`.

Logs: `RUST_LOG=info,gorca_daemon::plugins=debug` adds `plugin call exec_us=... grant_us=...`, `plugin fact`
and message-catalog misses. Faults and masks are WARN already; the drop-in is only needed to see timings.
## Update 2026-10-26

The unit-side workaround (dropping the namespace options) is superseded: the full sandbox is back and identity comes from the identity helper, see `docs/reports/2026-10-26-linux-platform-identity-helper.md`.

## Follow-up 3 (no faults, role facts logged, but no name on the chat rows)

The plugin matched the live rows all along; the names were dropped after the plugin returned them.

* `crates/gorca-daemon/tests/live_document.rs` (ignored, `GORCA_LIVE_DOC=1`, read-only) drives the
  real `AtspiDocSource` against the owner's Firefox over a peer-to-peer connection, builds rows the
  way the document actor does and runs the real plugin in the doc actor's batch order. Result on the
  live Telegram A document: 30 `a.ListItem-button` rows (role 31, link), `h3.fullName`, `span.time`,
  `p.last-message`, hashed badge classes, text runs as separate leaves; 30 name facts
  ("Артем, Сам как?, 25 авг.", "... , 1 unread, ...") and one article per `Message`; labels and
  sps schedulers alike. So the watch (the row node, class `ListItem-button`) and the structure are right.
* The installed package has `i18n/` as `drwxr-x--- root root`: the user's gorca cannot read the
  catalogs, `register_catalogs` skipped them silently, and every message fact of the plugin (the
  chat names) resolved to nothing and was dropped. Role facts are plain codes, so they survive: that is
  exactly the journal (`group=0 value=code(5)` only, no `text(len=..)`). Reproduced by running the same
  probe against a copy of `plugins/dist` without `i18n`: 0 name facts, article facts intact.
* Immediate fix for the owner: `sudo chmod 755 /usr/lib/gorca/addons/gorca.telegram-web/i18n`
  (files are already 644), then restart gorca. No new wasm: the pin is unchanged (`1c2fc587...`).
* Code: `register_catalogs` now warns (path, error, how to fix) for a catalog that exists but cannot be
  read, the first unresolved message of a plugin is a WARN (later ones stay debug), and
  `tools/build-plugins.sh` makes `plugins/dist/<id>` world-readable so a root copy keeps the modes.
* Fixture `tests/fixtures/web/telegram-a-live.json` (anonymised: three chat rows and two messages as
  the real source delivered them) and the test `the_rows_the_real_source_delivered_for_live_web_a_...`
  keep a wrong panel from passing. `live_document.rs` regenerates it (`GORCA_DUMP_FIXTURE=<path>`).
* Not done: a spoken end-to-end run in a private session (the fact-to-speech path is the fusion's
  Patch-over-observed rule, covered by the `fuse` tests `a_plugin_patch_is_in_the_first_published_entry...`).

## Follow-up 4: message list, headings, click handlers (owner requests)

Facts only (role, level and description patches, names from the plugin catalog); no DOM injection.
Structure read from the owner's open chat (read-only dumps, 2026-10-26); fixture
`tests/fixtures/web/telegram-a-open-chat.json` is the real source output, anonymised.

* Live shape: `div.messages-layout` holds the header title (`h3.fullName`, role button, a direct
  child; there is no `MiddleHeader`/`ChatInfo` in the tree) and `div.Transition > div.MessageList`,
  whose direct children are `div.sticky-date` separators and `div.Message.message-list-item`
  (role section, no AT-SPI action; outgoing ones carry the class `own`). Replies
  (`div.EmbeddedMessage`) have the action `click`, their children `clickAncestor`.
* **Messages as a list**: `MessageList` becomes `list` named "Messages" / "Сообщения"; each
  `div.Message` becomes `listitem`. Chosen over `article`: the list is announced once and each item then
  carries a position ("3 of 40"; the messages are direct children of the list, so counting is exact),
  while `article` says the word on every message and has no list context; quick-nav keeps working
  (`L` list, `I` item); `A` no longer stops on messages. A role patch is the only handle the fact API
  gives (`Value::Position/Grid` are not admitted).
* **Own messages**: a message with class `own` gets the description "Your message" / "Ваше сообщение"
  (catalog `own-message`), so the owner hears who wrote it without a name that would double the text.
  Remove the `own` branch of `message_row` if it reads badly.
* **Chat header**: the watch is `messages-layout`; its direct-child `h3.fullName` becomes a level-1
  `heading` (matched by structure; chat-list titles are below `a.ListItem-button` and untouched).
* **Date separators** (`div.sticky-date`) become level-2 headings. The **unread divider** rule
  (`.unread-divider` / `.local-action-message`) is kept but its class names are NOT confirmed: no chat
  with unread messages has been dumped; the test uses a synthetic divider.
* Host fix found on the way: a fact on a node the call only read (the header title) was admitted once
  per target of the batch; it is now admitted once (`admit_done`, the first target claims it).

### Click handlers (recorded only, nothing implemented)

Gecko exposes a real click handler as an action on the node that owns it and `clickAncestor` on its
descendants. Chat list: `a.ListItem-button` (role link, `jump`) opens the chat, `h3.fullName`
(`press`) and `button.Button` (`press`); everything inside a row has `clickAncestor` only.
Open chat: `div.Message` has NO action (a message itself has no mouse-only handler); the only
`click` seen is on `div.EmbeddedMessage` (the quoted reply inside a message: jumps to the replied
message), its text and sender spans being `clickAncestor`. Menus (`message-action-buttons`,
context menu) were not dumped.

### Landmarks (owner request, same slice)

Matched by the structure of the live open chat (read-only dump, ids/classes as Gecko exposes them):

| Region | Node (watch) | Role / catalog name | Covers |
|---|---|---|---|
| Chats | `div.chat-list` | navigation, "Chats" / "Чаты" | the chat rows (not the left header with menu and search, not the folder tabs) |
| Messages | the parent of `MessageList` (an unclassed `div.Transition`, reached with `model.parent`) | region, "Messages" / "Сообщения" | the message list and, below it, the composer's footer |
| Message list | `div.MessageList` | list, "Message list" / "Список сообщений" | the messages and date headings; keeps the N-of-M count |
| Composer | `div.Composer` | region, "Message input" / "Поле ввода" | text field, attach, emoji, send/record buttons |

* **Header**: Gecko flattens it: the avatar, title, status and the search/call/menu buttons are direct
  children of `div.messages-layout`; there is no node that covers them, so a banner/region cannot be
  patched onto it without marking something that also covers the messages. It stays reachable as the
  level-1 heading (`H`) which is the title; the buttons follow it in reading order.
* The roles are ordinary fused-model roles: `flags_from_sem` turns navigation/region into the landmark
  category the browse quick-nav (`D`) and the "where am I" ancestry read, and the verbosity rules apply to them
  like to authored landmarks. That wiring is by design and covered by existing tests, not exercised
  end to end with this plugin (no private-session run). Note: a region without `ROLE_AUTHORED` still
  counts as a landmark (`Cat::LANDMARK`), which `D` takes.
* Test: `the_open_chat_gets_a_message_list_headings_and_described_own_messages` now also checks the
  three landmarks on the real open-chat fixture (the dump includes the composer footer and the chat-list node).
