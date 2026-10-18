#!/usr/bin/python3
"""Regression for the Mutter 48.7 NULL deref (rebuild_all_grabbed_modifiers) with gorca's REAL
key source (crates/gorca-input-mutter, example key_session) instead of raw D-Bus calls.

Run inside the private shell world (never the live session):
  tests/shell/run_shell.sh -- /usr/bin/python3 tools/platform-probes/shell-segfault/regress.py \
      --bin target/debug/examples/key_session --mode stall2 --runs 20

Modes (analogues of repro.py; det/setg need a non-gorca NULL-modifier client, so here they are
the gorca-side equivalents):
  stall      stop the shell, gorca A queues a SetKeyGrabs, A is SIGKILLed, gorca B starts, cont
  stall2     as stall, A leaves ~40 queued grab changes (SetKeyGrabs + Grab/UngrabKeyboard)
  stallkeep  stop the shell for longer than the grab deadline: A must keep its ONE connection
             (no session restart), then everything is applied after cont
  det        A active, A killed, B starts at once
  setg       A active, B starts while A holds the name: B must not call the manager at all

NOTE: the stall modes SIGSTOP the whole Shell (all threads). That models a stopped process, not a
main-thread-only stall; there Peer.Ping is still answered by GDBus' worker thread.

Only PIDs started here and GORCA_SHELL_PID (recorded by the rig) are signalled, never by name.
Pass = the private shell is alive after each run and the expected gorca behaviour holds.
"""
import argparse
import json
import os
import signal
import subprocess
import sys
import tempfile
import time

ACTIVE = "keyboard monitor active"
LOST = "keyboard monitor lost"


def shell_alive(pid):
    try:
        with open(f"/proc/{pid}/stat") as f:
            return f.read().rsplit(")", 1)[1].split()[0] not in ("Z", "X")
    except OSError:
        return False


class Gorca:
    def __init__(self, binary, addr, tag, outdir):
        self.path = os.path.join(outdir, f"{tag}.log")
        self.log = open(self.path, "w+")
        self.p = subprocess.Popen([binary, addr], stdin=subprocess.PIPE, stderr=self.log,
                                  stdout=subprocess.DEVNULL, text=True)

    def text(self):
        self.log.flush()
        with open(self.path) as f:
            return f.read()

    def count(self, needle):
        return self.text().count(needle)

    def wait_active(self, timeout):
        end = time.time() + timeout
        while time.time() < end:
            if self.count(ACTIVE):
                return True
            if self.p.poll() is not None:
                return False
            time.sleep(0.05)
        return False

    def send(self, line):
        try:
            self.p.stdin.write(line + "\n")
            self.p.stdin.flush()
        except OSError:
            pass

    def kill(self):
        if self.p.poll() is None:
            self.p.kill()
        self.p.wait()

    def quit(self):
        self.send("quit")
        try:
            self.p.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.kill()


def run_once(mode, binary, addr, shell, outdir, i):
    res = {"mode": mode, "run": i, "load": open("/proc/loadavg").read().split()[0]}
    a = Gorca(binary, addr, f"{mode}-{i}-A", outdir)
    gs = [a]
    ok = a.wait_active(30)
    res["a_active"] = ok
    if not ok:
        res["fail"] = "A never became active"
        return res, gs
    try:
        if mode in ("stall", "stall2"):
            os.kill(shell, signal.SIGSTOP)
            n = 40 if mode == "stall2" else 1
            for k in range(n):
                a.send(f"all {k % 2}" if mode == "stall2" else "chord 1")
                a.send(f"chord {k + 1}")
            time.sleep(0.3)
            a.kill()
            b = Gorca(binary, addr, f"{mode}-{i}-B", outdir)
            gs.append(b)
            time.sleep(0.3)
            os.kill(shell, signal.SIGCONT)
            res["b_active"] = b.wait_active(40)
        elif mode == "stallkeep":
            os.kill(shell, signal.SIGSTOP)
            for k in range(40):
                a.send(f"all {k % 2}")
                a.send(f"chord {k + 1}")
            time.sleep(7.0)
            os.kill(shell, signal.SIGCONT)
            time.sleep(3.0)
            res["a_lost"] = a.count(LOST)
            res["a_sessions"] = a.count(ACTIVE)
            res["a_timeouts"] = a.count("timed out")
            if res["a_lost"] or res["a_sessions"] != 1:
                res["fail"] = "A restarted its session on a mere stall"
        elif mode == "det":
            a.kill()
            b = Gorca(binary, addr, f"{mode}-{i}-B", outdir)
            gs.append(b)
            res["b_active"] = b.wait_active(40)
        elif mode == "setg":
            b = Gorca(binary, addr, f"{mode}-{i}-B", outdir)
            gs.append(b)
            time.sleep(3.0)
            res["b_active_while_held"] = bool(b.count(ACTIVE))
            if res["b_active_while_held"]:
                res["fail"] = "B became active while A holds the name"
            a.quit()
            res["b_active"] = b.wait_active(30)
        else:
            raise SystemExit(f"unknown mode {mode}")
    finally:
        try:
            os.kill(shell, signal.SIGCONT)
        except OSError:
            pass
    time.sleep(2.0)
    res["shell_alive"] = shell_alive(shell)
    if not res["shell_alive"]:
        res["fail"] = "SHELL CRASHED"
    elif res.get("b_active") is False:
        res["fail"] = "B never became active"
    return res, gs


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--mode", required=True)
    ap.add_argument("--runs", type=int, default=1)
    ap.add_argument("--out", default=None)
    args = ap.parse_args()
    addr = os.environ["DBUS_SESSION_BUS_ADDRESS"]
    shell = int(os.environ["GORCA_SHELL_PID"])
    outdir = args.out or tempfile.mkdtemp(prefix="regress-", dir=os.environ.get("GORCA_HARNESS_DIR"))
    os.makedirs(outdir, exist_ok=True)
    print(f"logs in {outdir}", flush=True)
    fails = 0
    for i in range(args.runs):
        res, gs = run_once(args.mode, os.path.abspath(args.bin), addr, shell, outdir, i)
        for g in gs:
            g.quit() if g.p.poll() is None else g.kill()
        print(json.dumps(res), flush=True)
        if res.get("fail"):
            fails += 1
        if not shell_alive(shell):
            break
        time.sleep(1.0)
    print(f"RESULT mode={args.mode} runs={args.runs} failures={fails}", flush=True)
    sys.exit(1 if fails else 0)


main()
