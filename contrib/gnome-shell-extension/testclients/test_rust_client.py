#!/usr/bin/python3
# End-to-end: the Rust ShellHelperClient (examples/pointer_cli, copied to WORKDIR/rs-a by test-rig.sh with
# GORCA_RIG_EXTRA_EXE) against the REAL helper in the private shell. Python moves the pointer over a GTK4 client
# and checks what the application received; the Rust process does the clicks and the drag lock.
import os, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ptestlib import *

W = os.environ['W']
HERE = os.path.dirname(os.path.abspath(__file__))
R = Results()
RS = os.path.join(W, 'rs-a')


class Pointer:
    """The Rust client as a subprocess with a line protocol."""
    def __init__(self):
        self.p = subprocess.Popen([RS, BUS], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1)
        with open(os.path.join(W, 'pids'), 'a') as f:
            f.write(f'{self.p.pid}\n')
        self.first = self.p.stdout.readline().strip()

    def cmd(self, line):
        self.p.stdin.write(line + '\n')
        self.p.stdin.flush()
        return self.p.stdout.readline().strip()

    def quit(self):
        self.p.stdin.write('quit\n')
        self.p.stdin.flush()
        out = self.p.stdout.read()
        self.p.wait(5)
        return out.strip()


probe('HideOverview')
time.sleep(0.6)
c = Client(['/usr/bin/python3', os.path.join(HERE, 'gtk4_clicker.py'), 'RC'], name='rc')
wait_for(lambda: 'READY' in c.text(), 20)
time.sleep(1.5)
w = find_windows(title='RC')[0]
probe('MoveFrame', w['id'], 150, 150)
time.sleep(1.5)
w = find_windows(title='RC')[0]


def at(x, y):
    """Move the helper's pointer over window coordinates (x, y) with the Python (pointer-cap) caller."""
    r = move(w['id'], 'frame', x, y)
    assert r['outcome'] == 'done', r
    time.sleep(0.25)


ptr = Pointer()
R.check('Rust client reaches Ready against the real helper', 'Ready' in ptr.first, ptr.first)
if 'Ready' not in ptr.first:
    print(ptr.first)
    sys.exit(R.summary() or 1)

# click at the pointer, every button, at the exact spoken position
for letter, num in (('L', '1'), ('M', '2'), ('R', '3')):
    at(180, 220)
    n0 = len(c.events())
    r = ptr.cmd(f'click {letter}')
    ok = wait_for(lambda: len(c.events()) >= n0 + 2, 3)
    e = c.events()[n0:n0 + 2]
    R.check(f'click {letter} lands at the pointer (180,220) as button {num}',
            ok and r.startswith('Ok') and e[0][0] == 'press' and e[0][1] == num and abs(float(e[0][2]) - 180) <= 1 and abs(float(e[0][3]) - 220) <= 1 and e[1][0] == 'release',
            f'{r} {e}')

# the pointer position the Rust side reads equals where Python put it
at(200, 260)
r = ptr.cmd('pos')
R.check('position read through the Rust client', 'x: 350.0' in r and 'y: 410.0' in r, r)

# drag lock: press here, move, release there (same process = same holder)
at(160, 200)
n0 = len(c.events())
r = ptr.cmd('press L')
R.check('press L reports the button held', 'held: 1' in r, r)
at(300, 330)
r = ptr.cmd('release L')
ok = wait_for(lambda: any(x[0] == 'release' for x in c.events()[n0:]), 3)
e = c.events()[n0:]
R.check('drag: press at (160,200), release at (300,330)',
        ok and e[0][0] == 'press' and abs(float(e[0][2]) - 160) <= 1 and e[-1][0] == 'release' and abs(float(e[-1][2]) - 300) <= 1 and abs(float(e[-1][3]) - 330) <= 1 and 'held: 0' in r,
        f'{r} {e}')

# graceful shutdown releases a held button (the connection is shared, the helper never sees it vanish)
at(170, 210)
n0 = len(c.events())
ptr.cmd('press L')
out = ptr.quit()
ok = wait_for(lambda: any(x[0] == 'release' for x in c.events()[n0:]), 3)
R.check('shutdown of the client releases a held button', ok, f'{out} {c.events()[n0:]}')

# a second client process while the first holds: busy
p1 = Pointer()
at(170, 210)
p1.cmd('press L')
p2 = Pointer()
r = p2.cmd('press R')
R.check('another holder is refused while a button is held', 'Busy' in r, r)
p2.quit()
p1.quit()

# Rust double click
ptr = Pointer()
at(190, 230)
n0 = len(c.events())
ptr.cmd('dclick L')
ok = wait_for(lambda: len(c.events()) >= n0 + 4, 3)
R.check('double click delivers two presses', ok and sum(1 for x in c.events()[n0:] if x[0] == 'press') == 2, str(c.events()[n0:]))
ptr.quit()
c.kill()
sys.exit(R.summary())
