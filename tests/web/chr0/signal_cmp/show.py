#!/usr/bin/python3 -I
"""Print a driver run (steps.json) as a compact table: step, key, consumed/passed, focus events, speech."""
import json, sys

rows = json.load(open(sys.argv[1] + '/steps.json'))
for label, arg, r, fd, said in rows:
    ev = fd.split('EVENTS:')[1].strip() if 'EVENTS:' in fd else ''
    evs = [e.strip() for e in ev.split('||') if e.strip() and 'children-changed' not in e.split(' ')[0]]
    short = []
    for e in evs:
        parts = e.split(' ', 2)
        short.append(parts[0] + ' ' + (parts[2][:60] if len(parts) > 2 else ''))
    cons = r.get('consumed')
    print('%-7s %-7s consumed=%-5s passed=%-5s focus-events=%d %s' % (label, arg, cons, r.get('passed'), len(evs), '; '.join(short)[:130]))
    for t, v, x in said:
        print('        %-5s %s' % (v, x[:140]))
