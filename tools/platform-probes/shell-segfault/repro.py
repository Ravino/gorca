#!/usr/bin/python3
"""Repro for the Mutter 48.7 a11y-manager NULL deref (rebuild_all_grabbed_modifiers).
Run inside tests/shell/run_shell.sh (private world, --unsafe-mode). mode:
  det    A: Watch+SetKeyGrabs; B: Watch only; drop A  -> vanished callback walks B (modifiers NULL)
  stall  gorca-like: stop shell (our private pid), A: Watch+SetKeyGrabs queued, drop A,
         B: Watch + SetKeyGrabs queued, cont
  setg   A: Watch only, B: SetKeyGrabs  -> no disconnect needed
"""
import os, sys, time, signal
import gi
gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib

mode = sys.argv[1]
addr = os.environ["DBUS_SESSION_BUS_ADDRESS"]
pid = int(os.environ["GORCA_SHELL_PID"])
N, P, I = "org.freedesktop.a11y.Manager", "/org/freedesktop/a11y/Manager", "org.freedesktop.a11y.KeyboardMonitor"
FL = Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION

def conn():
    return Gio.DBusConnection.new_for_address_sync(addr, FL, None, None)

def call(c, m, args=None, sync=True):
    v = GLib.Variant("()", ()) if args is None else args
    if sync:
        return c.call_sync(N, P, I, m, v, None, 0, 5000, None)
    c.call(N, P, I, m, v, None, 0, 5000, None, lambda *a: None)

def alive():
    try:
        os.kill(pid, 0)
        return "S" not in "" and open(f"/proc/{pid}/stat").read().split()[2] != "Z"
    except OSError:
        return False

SG = GLib.Variant("(aua(uu))", ([], []))
a = conn()
if mode == "det":
    call(a, "WatchKeyboard"); call(a, "SetKeyGrabs", SG)
    b = conn(); call(b, "WatchKeyboard")
    print("A has modifiers, B watch-only; closing A", flush=True)
    a.close_sync(None)
elif mode == "setg":
    call(a, "WatchKeyboard")
    b = conn(); print("B SetKeyGrabs", flush=True); call(b, "SetKeyGrabs", SG)
elif mode == "stall2":
    # like stall, but A leaves many fire-and-forget calls in flight when it disconnects
    call(a, "WatchKeyboard"); call(a, "SetKeyGrabs", SG)
    os.kill(pid, signal.SIGSTOP)
    for _ in range(40):
        call(a, "UngrabKeyboard", sync=False)
    time.sleep(0.3)
    a.close_sync(None)
    b = conn()
    call(b, "WatchKeyboard", sync=False); call(b, "SetKeyGrabs", SG, sync=False)
    time.sleep(0.3)
    os.kill(pid, signal.SIGCONT)
elif mode == "stall":
    call(a, "WatchKeyboard"); call(a, "SetKeyGrabs", SG)
    os.kill(pid, signal.SIGSTOP)
    call(a, "SetKeyGrabs", SG, sync=False)
    time.sleep(0.3)
    a.close_sync(None)
    b = conn()
    call(b, "WatchKeyboard", sync=False); call(b, "SetKeyGrabs", SG, sync=False)
    time.sleep(0.3)
    os.kill(pid, signal.SIGCONT)
time.sleep(2.5)
print("shell alive after:", alive(), flush=True)
