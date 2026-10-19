#!/usr/bin/python3
# S-6 / M-7: handler main-loop time of the pointer methods (pick-based occlusion included) and client round trips.
# Needs a rig started with GORCA_TRACE=1 (helper logs "trace <Method> handler_us=N"). Usage: bench_pointer.py [N] [WINDOWS]
import re, statistics, sys
sys.path.insert(0, __import__('os').path.dirname(__import__('os').path.abspath(__file__)))
from ptestlib import *

N = int(sys.argv[1]) if len(sys.argv) > 1 else 200
NW = int(sys.argv[2]) if len(sys.argv) > 2 else 6
HERE = os.path.dirname(os.path.abspath(__file__))
W = os.environ['W']
probe('HideOverview')
cl = []
for i in range(NW):
    c = Client(['/usr/bin/python3', os.path.join(HERE, 'gtk4_clicker.py'), f'B{i}'], name=f'b{i}')
    wait_for(lambda: 'READY' in c.text(), 20)
    cl.append(c)
time.sleep(2)
ws = [find_windows(title=f'B{i}')[0] for i in range(NW)]
top = ws[-1]
log0 = open(os.path.join(W, 'shell.log')).read().count('\n')


def run(label, fn):
    rt = []
    for i in range(N):
        t0 = time.perf_counter()
        try:
            fn(i)
        except HelperError as e:
            if not e.msg.startswith('rate'):
                raise
            time.sleep(0.2)
            continue
        rt.append((time.perf_counter() - t0) * 1e6)
        time.sleep(0.06)
    rt.sort()
    print(f'{label}: n={len(rt)} client RTT us p50={rt[len(rt) // 2]:.0f} p99={rt[int(len(rt) * .99) - 1]:.0f} max={rt[-1]:.0f}')


run('PointerMove(window, frame)', lambda i: move(top['id'], 'frame', 100 + i % 50, 100))
run('PointerClickAt(window, frame, button 3)', lambda i: click_at(top['id'], 'frame', 100 + i % 50, 120, 3))
run('GetPointer', lambda i: get_pointer())
run('PointerMove(stage 0)', lambda i: move(0, 'stage', 300 + i % 20, 10))
time.sleep(0.5)
lines = open(os.path.join(W, 'shell.log')).read().split('\n')[log0:]
by = {}
for l in lines:
    m = re.search(r'trace (\w+) handler_us=(\d+)', l)
    if m:
        by.setdefault(m.group(1), []).append(int(m.group(2)))
    m = re.search(r'trace pick_us=(\d+)', l)
    if m:
        by.setdefault('pick (get_actor_at_pos + owner walk)', []).append(int(m.group(1)))
for k, v in by.items():
    v.sort()
    print(f'handler {k}: n={len(v)} p50={v[len(v) // 2]} us p99={v[int(len(v) * .99) - 1]} us max={v[-1]} us')
for c in cl:
    c.kill()
