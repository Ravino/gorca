#!/usr/bin/python3
# Deny-path flood isolation. Usage: test_flood.py flood   (role c: many connections hammer the helper for DURATION s)
#                                 test_flood.py victim  (role a: an established allowed caller keeps working)
import os, statistics, sys, threading, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ptestlib import *

DURATION = 6.0
mode = sys.argv[1]
if mode == 'flood':
    stop = time.time() + DURATION
    counts = {'denied': 0, 'rate': 0}

    def one():
        c = new_conn()
        while time.time() < stop:
            try:
                call('Ping', rsig='(u)', c=c)
            except HelperError as e:
                counts['denied' if e.name == 'AccessDenied' else 'rate'] += 1
    ts = [threading.Thread(target=one) for _ in range(60)]
    for t in ts:
        t.start()
    for t in ts:
        t.join()
    print('flood done', counts)
else:
    R = Results()
    c = new_conn()
    R.check('victim authorized before the flood', ping() == 2 and call('Ping', rsig='(u)', c=c) == 2)
    base = []
    for _ in range(100):
        t0 = time.perf_counter(); call('Ping', rsig='(u)', c=c); base.append((time.perf_counter() - t0) * 1e3)
        time.sleep(0.01)
    time.sleep(1.0)   # the flooder starts at the same time as this process; let it ramp up
    lat, errs = [], 0
    t_end = time.time() + 4.0
    while time.time() < t_end:
        t0 = time.perf_counter()
        try:
            call('Ping', rsig='(u)', c=c)
            lat.append((time.perf_counter() - t0) * 1e3)
        except HelperError:
            errs += 1
        time.sleep(0.02)
    lat.sort(); base.sort()
    p99 = lambda v: v[int(len(v) * 0.99) - 1]
    R.check('allowed caller keeps its authorization during the flood (0 errors)', errs == 0 and len(lat) > 100, f'errors={errs} calls={len(lat)}')
    R.check('allowed caller latency during the flood is not degraded (p99 < 10 ms and < 5x quiet p99 + 3 ms)',
            p99(lat) < 10 and p99(lat) < 5 * p99(base) + 3, f'quiet p50={base[len(base) // 2]:.2f} p99={p99(base):.2f} ms; flood p50={lat[len(lat) // 2]:.2f} p99={p99(lat):.2f} ms')
    # pointer calls still work too
    try:
        r = move(0, 'stage', 300, 10, c=c)
        R.check('allowed caller can still use the pointer API during the flood', r['outcome'] in ('done', 'refused-outside'), str(r))
    except HelperError as e:
        R.check('allowed caller can still use the pointer API during the flood', False, str(e))
    sys.exit(R.summary())
