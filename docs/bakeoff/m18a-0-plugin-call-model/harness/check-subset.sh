#!/usr/bin/env bash
# Proves harness/wit is a strict subset of wit/gorca-ext-0.10: every interface of the subset is
# token-identical (comments and whitespace ignored) to the interface of the same name in the
# original, types.wit is identical, and the subset world only imports/exports interfaces that
# the original `app-module` world imports/exports. Exit 0 = subset holds.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../../.." && pwd)"
exec python3 -I - "$root/wit/gorca-ext-0.10" "$here/wit" <<'PY'
import re, sys, glob, os
orig, sub = sys.argv[1], sys.argv[2]
def norm(t):
    t = re.sub(r'//[^\n]*', '', t)
    return re.sub(r'\s+', ' ', t).strip()
def blocks(txt):
    out = {}
    for m in re.finditer(r'^(interface|world) ([\w-]+) \{', txt, re.M):
        d = 0; j = m.end() - 1
        while True:
            if txt[j] == '{': d += 1
            elif txt[j] == '}':
                d -= 1
                if d == 0: break
            j += 1
        out[(m.group(1), m.group(2))] = norm(txt[m.start():j+1])
    return out
def load(d):
    allb = {}
    for f in sorted(glob.glob(d + '/*.wit')):
        allb.update(blocks(open(f).read()))
    return allb
o, s = load(orig), load(sub)
bad = 0
for k, v in s.items():
    if k[0] == 'interface':
        if o.get(k) != v:
            print("NOT A SUBSET: interface", k[1]); bad += 1
if norm(open(orig + '/types.wit').read()) != norm(open(sub + '/types.wit').read()):
    # types is an interface block too; the loop above covers it, this is the whole-file check
    print("types.wit differs"); bad += 1
w = s[('world', 'bakeoff-rules')]
ow = o[('world', 'app-module')]
for kind, name in re.findall(r'\b(import|export) ([\w-]+);', w):
    if f'{kind} {name};' not in ow:
        print("world item not in app-module:", kind, name); bad += 1
if bad: sys.exit(1)
print("subset OK:", ", ".join(sorted(k[1] for k in s)))
PY
