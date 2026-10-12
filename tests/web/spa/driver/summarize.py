#!/usr/bin/env python3
"""Print compact per-step tables from results/<browser>/*.json (input for the report)."""
import json, sys, glob, os
b = sys.argv[1]; pat = sys.argv[2] if len(sys.argv) > 2 else ''
for f in sorted(glob.glob(os.path.join(os.path.dirname(__file__), '..', 'results', b, '*.json'))):
    if f.endswith('.events.json') or pat not in os.path.basename(f): continue
    r = json.load(open(f))
    print(f"## {r['scenario']} nodes0={r['nodes_initial']} load_ev={r['load']['n_events']} load_last={r['load'].get('last_event_ms')}ms")
    for s in r['steps']:
        i = s['identity']
        top = ', '.join(f"{k.replace('Object.','').replace('Document.','')}={v}" for k, v in list(s['counts'].items())[:6])
        print(f"- {s['label']}: ev={s['n_events']} first={s['t_first_ms']} last={s['t_last_ms']} burst/100ms={s['max_events_per_100ms']} domrec={(s['dom_mutations'] or {}).get('recs')} | {top}")
        print(f"    silent={s.get('silent_structure_change')} reordered_parents={i['reordered_parents']} moved={i['children_moved']} reparented={i['reparented']} attr_changed={i['attr_changed']} state_changed={i['state_changed']}")
        print(f"    id: common={i['common_paths']}/{i['paths_before']} same={i['same_sig']} inplace={i['inplace_update']} recycled={i['path_recycled_other_node']} gone={i['gone']} new={i['new']} | domid {i['dom_id_path_kept']}/{i['dom_id_both']} kept | name {i['name_match_path_kept']}/{i['name_match_both']} | dom_focus={s['dom_focus']} atspi_focus={s['atspi_focus']}")
