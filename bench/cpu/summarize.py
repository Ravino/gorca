import re, sys
unit = {'ps': 1e-3, 'ns': 1.0, 'µs': 1e3, 'us': 1e3, 'ms': 1e6, 's': 1e9}

def parse(path):
    name = None
    res = {}
    for line in open(path):
        t = line.strip()
        m = re.match(r'^([a-z0-9-]+/\S+)\s*$', t)
        if m:
            name = m.group(1); continue
        m = re.match(r'^([a-z0-9-]+/\S+)\s+time:\s+\[\S+ \S+ (\S+) (\S+) \S+ \S+\]', t)
        if m:
            res[m.group(1)] = float(m.group(2)) * unit[m.group(3)]; name = None; continue
        m = re.match(r'^time:\s+\[\S+ \S+ (\S+) (\S+) \S+ \S+\]', t)
        if m and name:
            res[name] = float(m.group(1)) * unit[m.group(2)]; name = None
    return res

runs = [parse(p) for p in sys.argv[1:]]
best = {}
for r in runs:
    for k, v in r.items():
        best[k] = min(best.get(k, 1e30), v)
rows = {}
for k, v in best.items():
    parts = k.split('/')
    if parts[0] == 'dispatch-overhead':
        print(k, round(v, 2), 'ns'); continue
    rows.setdefault(tuple(parts[:-2]) + (parts[-1],), {})[parts[-2]] = v
print('| case | baseline | impls (ns, speedup vs baseline) |')
for key in sorted(rows, key=lambda k: (k[:-1], int(k[-1]))):
    r = rows[key]
    base_name = next((b for b in ('naive', 'before', 'portable') if b in r), None)
    p = r[base_name]
    out = '; '.join(f'{i} {t:.1f}' + (f' ({p / t:.2f}x)' if i != base_name else '') for i, t in r.items())
    print(f"| {'/'.join(key)} | {base_name} | {out} |")
