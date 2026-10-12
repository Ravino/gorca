# Platform probes (read-only measurements, 2026-10-12)

Run with the system `/usr/bin/python3` (needs `python3-gi`); none of them modifies the desktop.

* `cachebench.py <addr-file> :1.N ...` AT-SPI `Cache.GetItems`, `Peer.Ping`, `Properties.Get` timings on the a11y bus
  (address from `busctl --user call org.a11y.Bus /org/a11y/bus org.a11y.Bus GetAddress`).
* `termprobe.py <addr-file> :1.N` Text interface sizes/timings of a VTE terminal (reads, prints counts only).
* `pingbus.py` session-bus Peer.Ping RTT to gnome-shell / portal / Orca.
* `espeak_lat.c`, `espeak_cold.c` libespeak-ng synchronous first-sample latency
  (`gcc -O2 x.c -lespeak-ng`).
* `ssip_lat.py <socket>`, `ssip_prio.py <socket>` SSIP latency/priority tests against a PRIVATE speech-dispatcher:
  `speech-dispatcher -s -t 0 -c unix_socket -S <sock> -C <conf-copy-with-AudioOutputMethod "alsa" and AudioALSADevice "null" and an AddModule espeak-ng line> -P <pid> -L <logdir>`.
  Never point them at the user's daemon.
