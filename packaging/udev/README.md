# gorca udev rule: idle-state lease

`70-gorca-cpu-dma-latency.rules` gives the user on the active seat write access to
`/dev/cpu_dma_latency` (via logind's `uaccess` ACL). gorca uses it for the `[plugins] idle_lease`
setting (default `"auto"`): while plugins are active it holds a 20 microsecond latency request, so
the CPUs stay out of C2/C3 and a parked plugin worker wakes in tens of microseconds instead of
hundreds. The request is released 2 seconds after the last key; the kernel also drops it if gorca
dies. Without the rule gorca logs one `pm-qos-unavailable` line and does nothing.

Install (as root, by the owner):

    sudo install -m 0644 packaging/udev/70-gorca-cpu-dma-latency.rules /etc/udev/rules.d/
    sudo udevadm control --reload
    sudo udevadm trigger --subsystem-match=misc --sysname-match=cpu_dma_latency

Check: `getfacl /dev/cpu_dma_latency` lists your user. Remove: delete the file and repeat the
`udevadm` lines. Turn the feature off without removing the rule: `[plugins] idle_lease = "off"`.
