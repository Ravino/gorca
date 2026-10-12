#!/usr/bin/env python3
"""One-line-per-step comparison table across browsers/frameworks (report input)."""
import json, glob, os, sys
pat = sys.argv[1] if len(sys.argv) > 1 else ''
R = os.path.join(os.path.dirname(__file__), '..', 'results')
def short(counts, n=4):
    return ' '.join(f"{k.replace('Object.','').replace('Document.','').replace('ChildrenChanged','CC').replace('StateChanged','SC').replace('PropertyChange','PC').replace('TextChanged','TC').replace('/system','/s')}={v}" for k, v in list(counts.items())[:n])
for b in ('chrome', 'firefox'):
    for f in sorted(glob.glob(f'{R}/{b}/*.json')):
        if f.endswith('.events.json') or pat not in os.path.basename(f): continue
        r = json.load(open(f))
        for s in r['steps']:
            i = s['identity']
            print(f"{b[:2]} {r['scenario']:22} {s['label'][:26]:26} ev={s['n_events']:4} t1={s['t_first_ms']} tL={s['t_last_ms']} | {short(s['counts'])} | com={i['common_paths']}/{i['paths_before']} gone={i['gone']} new={i['new']} inpl={i['inplace_update']} recyc={i['path_recycled_other_node']} reord={i['reordered_parents']} | id {i['dom_id_path_kept']}/{i['dom_id_both']} | silent={int(bool(s.get('silent_structure_change')))}")
