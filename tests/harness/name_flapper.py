#!/usr/bin/python3
"""name_flapper.py - flaps ownership of org.freedesktop.a11y.Manager on the private session bus.

Usage: name_flapper.py SESSION_ADDRESS [FLAPS] [SEED]

Each flap opens a connection, takes the manager's well-known name (DO_NOT_QUEUE, so it does
nothing while a real or emulated manager owns it), keeps it for 0 to 5 ms and drops the
connection. A screen reader connecting meanwhile sees the owner change under its feet; it must
end up trusting the real owner. Refuses to run outside a private harness session bus.
"""
import random
import sys
import time

import dbus
import dbus.bus

NAME = "org.freedesktop.a11y.Manager"


def main():
    addr = sys.argv[1]
    flaps = int(sys.argv[2]) if len(sys.argv) > 2 else 100
    rng = random.Random(int(sys.argv[3]) if len(sys.argv) > 3 else 1)
    if "gorca-h." not in addr:
        sys.exit("refusing to run: the session bus is not a private harness bus")
    won = 0
    for _ in range(flaps):
        conn = dbus.bus.BusConnection(addr)
        try:
            if conn.request_name(NAME, dbus.bus.NAME_FLAG_DO_NOT_QUEUE) == dbus.bus.REQUEST_NAME_REPLY_PRIMARY_OWNER:
                won += 1
                time.sleep(rng.uniform(0, 0.005))
        finally:
            conn.close()
        time.sleep(rng.uniform(0, 0.005))
    print(won)


main()
