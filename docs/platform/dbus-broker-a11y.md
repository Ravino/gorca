# dbus-broker and the accessibility bus

Verified: `ps` tree, `/usr/share/defaults/at-spi2/accessibility.conf`, `refs/dbus-broker/src/*`
(v37 on this host).

## Facts
* The a11y bus (`unix:path=/run/user/1000/at-spi/bus`) is its own dbus-broker started by
  `at-spi-bus-launcher` with `--config-file=/usr/share/defaults/at-spi2/accessibility.conf --scope user`
  (the launcher prefers dbus-broker, falls back to dbus-daemon; env override in
  `bus/at-spi-bus-launcher.c` ~l.666). Auth EXTERNAL only. Policy: everyone may own, send and
  receive anything.
* The config raises limits (same values as the stock session.conf): `max_incoming_bytes`/
  `max_outgoing_bytes`/`max_message_size` = 1,000,000,000, `max_match_rules_per_connection` 50000,
  `max_names_per_connection` 50000, `max_replies_per_connection` 50000, `reply_timeout` 300000 ms,
  `max_connections_per_user` 100000. dbus-broker-launch turns these into ONE per-uid pool:
  `--max-bytes = max_connections_per_user * max_outgoing_bytes` (`launcher.c` ~l.1115), i.e. effectively
  unlimited (compiled defaults without a config are 64 conns x 8 MiB, 256 match rules).
* There is no D-Bus "stats" interface: `org.freedesktop.DBus.Debug.Stats.GetAllMatchRules` returns
  "Unimplemented functionality" (live). Use `org.freedesktop.DBus.Monitoring.BecomeMonitor` for traces.
* `Registry.GetRegisteredEvents` is the only way to see who listens to what (38 rows for Orca live).

## Behaviour that shapes the architecture
1. **A slow receiver is never throttled or dropped silently; it is disconnected only when its quota is
   exhausted.** For broadcasts the broker queues each matching signal in the receiver's socket queue;
   if queueing fails on quota it calls `connection_shutdown(receiver)` and logs "Peer :1.N is being
   disconnected as it does not have the resources to receive a signal it subscribed to"
   (`src/bus/driver.c` `driver_forward_broadcast`; `src/catalog/dbus-broker.catalog`: "Disconnecting
   the peer ensures that no messages are silently dropped"). On this host's a11y bus the quota is the
   per-uid pool above, so in practice a stalled gorca makes **dbus-broker's memory grow** (up to the
   pool) and gorca then lags arbitrarily far behind the live UI; there is no backpressure on senders.
   gorca must drain its socket continuously on a task that never does anything slow, and implement
   its own bounded queues with coalescing/drop *after* reading. The session bus has the same limits.
2. **Same-uid peers are not protected from each other.** Quotas are per-uid fair shares across *users*
   (`src/util/user.c`: "never apply quotas on self-charge"), so one flooding app of the same user is
   only bounded by the global per-uid pool, not by a per-sender share. A chatty app cannot be throttled
   at the broker; throttle on the gorca side per sender.
3. Replies to method calls are queued per receiver and can be lost only by disconnect; a call to a hung
   app simply times out at the caller (libatspi: 800 ms / 15 s startup; zbus: set your own).
4. Broadcast routing is by match rule at the broker, so narrow rules (`sender=`, `member=`,
   `arg0path=` for minor types like `insert`) reduce load before data reaches gorca. Match-rule
   changes are `AddMatch` round trips to the broker.
5. Unicast signals (Mutter `KeyEvent`) are addressed to a unique name and are not subject to
   match-rule routing, only to the receiver queue quota (rule 1 applies on the session bus too).
