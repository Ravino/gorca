#!/usr/bin/env python3
"""Mutation/event characterization + tree dumps for the SPA corpus.

usage (via run_characterize.sh, which sets up the private a11y env):
  characterize.py --browser chrome|firefox [--only react/keyed,vue/virtual] [--base http://127.0.0.1:8089] [--out DIR]
  characterize.py --browser chrome --static          # large static pages (gen_large.py) + mutation helpers
Writes <out>/<browser>/<fw>__<scenario>.json (+ .events.json.gz raw) and dumps to research/web-graph/real-trees/.
"""
import sys, os, json, time, gzip, argparse, collections
HERE = os.path.dirname(os.path.abspath(__file__)); ROOT = os.path.abspath(os.path.join(HERE, '..', '..', '..', '..'))
sys.path.insert(0, HERE)
import atspi_bus, browsers, scenarios
from analyze import identity, step_summary, compact_tree

def jdump(path, obj, gz=False):
    s = json.dumps(obj, separators=(',', ':'))
    if gz or len(s) > 900_000:
        path += '.gz'
        with gzip.open(path, 'wt') as f: f.write(s)
    else:
        with open(path, 'w') as f: f.write(s)
    return path

DOM_STATE_JS = "(()=>{const a=document.activeElement;return a?(a.tagName.toLowerCase()+(a.id?'#'+a.id:'')+(a.getAttribute('aria-activedescendant')?' ->'+a.getAttribute('aria-activedescendant'):'')):null})()"
INSTALL_MUT = ("(()=>{const m=window.__gm={recs:0,added:0,removed:0,attrs:0,text:0};new MutationObserver(rs=>{for(const r of rs){m.recs++;"
               "if(r.type==='childList'){m.added+=r.addedNodes.length;m.removed+=r.removedNodes.length}else if(r.type==='attributes')m.attrs++;else m.text++}})"
               ".observe(document,{subtree:true,childList:true,attributes:true,characterData:true});return true})()")
READ_MUT = "(()=>{const m=window.__gm; if(!m) return null; const r={...m}; m.recs=m.added=m.removed=m.attrs=m.text=0; return r})()"

def do(br, step):
    op, a = step['op'], step['args']
    if op == 'click': br.click(a[0])
    elif op == 'key': br.key(a[0])
    elif op == 'type': br.type(a[0])
    elif op == 'wheel': br.wheel(a[0], a[1])
    elif op == 'eval': br.eval(a[0])
    else: raise ValueError(op)

def settle_doc(bus, br, app, dump_quiet=1.0):
    """Wait for load events to quiet, find the showing document, return (doc, nodes)."""
    time.sleep(0.3)
    m = bus.mark(); bus.wait_quiet(m, quiet=dump_quiet, maxwait=15)
    doc = bus.find_document(app)
    if not doc: raise RuntimeError('no document')
    return doc

def run_scenario(bus, br, app, key, url, steps, base, outdir, dump_dir, bname, dump_initial, dump_after):
    t_load0 = time.monotonic(); m_load = bus.mark()
    br.goto(url if url.startswith('file:') else base + url)
    last_ms, settled = bus.wait_quiet(m_load, quiet=1.0, maxwait=20, first_wait=1.5)
    load_events = bus.since(m_load)
    doc = bus.find_document(app)
    if not doc: raise RuntimeError('no document for ' + key)
    br.eval(INSTALL_MUT)
    if bname == 'firefox':   # Gecko escalates cache domains lazily: first Attributes/Component queries can come back empty
        bus.snapshot(doc); time.sleep(0.5)
    A = bus.snapshot(doc)
    # stability: second snapshot to detect late growth
    meta = {'browser': bname, 'scenario': key, 'url': url, 'fallback_doc_not_showing': bus.fallback_doc}
    if dump_initial:
        jdump(os.path.join(dump_dir, f"{bname}__{key.replace('/', '-')}__initial.json"), compact_tree(A, dict(meta, state='initial')))
    res = {'scenario': key, 'url': url, 'browser': bname, 'nodes_initial': len(A),
           'load': step_summary(load_events, t_load0, A, A), 'steps': []}
    res['load']['last_event_ms'] = None if last_ms is None else round(last_ms * 1000)
    raw = {'load': [list(map(lambda x: x if not isinstance(x, float) else round(x - t_load0, 4), e)) for e in load_events]}
    for s in steps:
        m = bus.mark(); br.eval(READ_MUT); t0 = time.monotonic()
        br.synthetic_scroll = False
        do(br, s)
        last_ms, settled = bus.wait_quiet(m, quiet=s['quiet'], maxwait=s['maxwait'])
        evs = bus.since(m)
        dom_mut = br.eval(READ_MUT); dom_focus = br.eval(DOM_STATE_JS); vis = br.eval("document.visibilityState+','+document.hasFocus()")
        if s['setup']:
            A = bus.snapshot(doc) if evs else A
            continue
        t_snap = time.monotonic()
        # the doc object can be replaced by a navigation (history.back may reload) -> re-find if it vanished
        try:
            B = bus.snapshot(doc, bounds=(dump_after and s is steps[-1]))
            if not B: raise RuntimeError('empty')
        except Exception:
            doc = bus.find_document(app); B = bus.snapshot(doc, bounds=False)
        r = {'label': s['label'], 'op': s['op'], 'args': s['args'], 'settled': settled, 'settle_wait_cap_s': s['maxwait'],
             'snapshot_s': round(time.monotonic() - t_snap, 2), 'dom_mutations': dom_mut, 'dom_focus': dom_focus,
             'nodes_before': len(A), 'nodes_after': len(B), 'page_visible_focus': vis, 'synthetic_scroll': getattr(br, 'synthetic_scroll', False)}
        r.update(step_summary(evs, t0, A, B))
        r['identity'] = identity(A, B)
        i = r['identity']; ev_struct = sum(v for k, v in r['counts'].items() if k.startswith('Object.ChildrenChanged') or 'Reorder' in k or k.startswith('Object.RowReordered'))
        r['structure_changed'] = i['new'] + i['gone'] + i['reordered_parents'] + i['reparented']
        r['silent_structure_change'] = bool(r['structure_changed']) and ev_struct == 0
        res['steps'].append(r)
        raw[s['label']] = [list(map(lambda x: x if not isinstance(x, float) else round(x - t0, 4), e)) for e in evs]
        A = B
    if dump_after:
        jdump(os.path.join(dump_dir, f"{bname}__{key.replace('/', '-')}__final.json"), compact_tree(A, dict(meta, state='after-steps')))
    os.makedirs(os.path.join(outdir, bname), exist_ok=True)
    fn = key.replace('/', '__')
    jdump(os.path.join(outdir, bname, fn + '.json'), res)
    jdump(os.path.join(outdir, bname, fn + '.events.json'), raw, gz=True)
    return res

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--browser', required=True); ap.add_argument('--only', default=''); ap.add_argument('--base', default='http://127.0.0.1:8089')
    ap.add_argument('--out', default=os.path.join(HERE, '..', 'results')); ap.add_argument('--static', action='store_true')
    ap.add_argument('--no-dumps', action='store_true'); ap.add_argument('--with-static', action='store_true')
    args = ap.parse_args()
    dump_dir = os.path.join(ROOT, 'research', 'web-graph', 'real-trees'); os.makedirs(dump_dir, exist_ok=True)
    bus = atspi_bus.Bus(os.environ['A11Y_ADDR']); bus.start_events()
    br = browsers.Chrome() if args.browser == 'chrome' else browsers.Firefox()
    app = bus.find_app('Chrom' if args.browser == 'chrome' else 'Fire', wait=40)
    if not app: raise SystemExit('browser did not register on the a11y bus')
    print('app', app, flush=True)
    try:
        todo = scenarios.all_scenarios()
        if args.static:
            todo = static_scenarios()
        if args.with_static:
            todo = {**todo, **static_scenarios()}
        if args.only:
            sel = set(args.only.split(',')); todo = {k: v for k, v in todo.items() if k in sel}
        for key, (url, steps) in todo.items():
            t = time.time()
            try:
                r = run_scenario(bus, br, app, key, url, steps, args.base, args.out, dump_dir, args.browser,
                                 (key in scenarios.DUMP or key.startswith('static/')) and not args.no_dumps,
                                 key in scenarios.DUMP_AFTER and not args.no_dumps)
                print(f"{key}: {r['nodes_initial']} nodes, {len(r['steps'])} steps, {time.time() - t:.1f}s", flush=True)
            except Exception as e:
                print(f'{key}: FAILED {type(e).__name__}: {e}', flush=True)
                app = bus.find_app('Chrom' if args.browser == 'chrome' else 'Fire', wait=5) or app
    finally:
        br.close()

def static_scenarios():
    d = os.environ.get('STATIC_DIR', '/tmp/static')
    mk = lambda n: [st for st in (scenarios.st(x, 'eval', x, quiet=1.0, maxwait=8) for x in n)]
    out = {}
    for n in (2000, 10000):
        out[f'static/large{n}'] = (f'file://{d}/large{n}.html', mk(['mut.rename(100)', 'mut.addCards(50)', 'mut.live(200)', 'mut.removeCards(20)', 'mut.route()']))
    out['static/frames'] = ('file://' + os.path.join(ROOT, 'tests/web/pages/frames/outer.html'), [])
    return out

if __name__ == '__main__':
    main()
