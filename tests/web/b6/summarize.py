#!/usr/bin/env python3
"""Summarise a run directory of the B-6 harness into Markdown tables.

usage: summarize.py bench/results/web-prep/run2 > bench/results/web-prep/run2/SUMMARY.md
Reads <run>/<browser>/*.json written by `b6 run`. Standard library only.
"""
import glob
import json
import os
import sys


def load(path):
    with open(path) as f:
        return json.load(f)


def f1(x):
    return '-' if x is None else f'{x:.1f}'


def build_tables(run):
    out = ['## Build (M-9 / WM-1)', '']
    for br in ('firefox', 'chromium'):
        files = sorted(glob.glob(f'{run}/{br}/*.build-{br}.json'), key=lambda p: (len(p), p))
        if not files:
            continue
        out += [f'### {br}', '', '| page | nodes | strategy | total p50 ms | p95 | structure ready | roles ready | GetTree | roles | states | calls | heaviest GetTree call ms |', '|---|---|---|---|---|---|---|---|---|---|---|---|']
        for f in files:
            d = load(f)
            for v in d['variants']:
                st = v['stages_ms']
                g = lambda k: st.get(k, {}).get('p50')
                out.append(f"| {d['page']} | {v['nodes']} | {v['variant']} | {f1(v['total_ms']['p50'])} | {f1(v['total_ms']['p95'])} | {f1(g('t_structure_ready'))} | {f1(g('t_roles_ready'))} | {f1(g('get_tree'))} | {f1(g('roles'))} | {f1(g('states'))} | {int(v['rpc_calls'])} | {f1(g('heaviest_get_tree_call'))} |")
        out.append('')
        out += ['First block (viewport first): time until the top of the document is complete', '', '| page | block nodes | subtrees left | p50 ms | p95 ms |', '|---|---|---|---|---|']
        for f in files:
            d = load(f)
            fb = d.get('first_block')
            if fb:
                out.append(f"| {d['page']} | {fb['block_nodes']} | {fb['subtrees_left']} | {f1(fb['ms']['p50'])} | {f1(fb['ms']['p95'])} |")
        out.append('')
        cold = [(load(f)['page'], load(f).get('first_block_cold')) for f in files]
        if any(isinstance(c, dict) for _, c in cold):
            out += ['Cold first block (fresh connection, cache domains not yet active; one sample per launch)', '', '| page | total ms | warm-up ms | frontier ms | GetTree ms | block nodes |', '|---|---|---|---|---|---|']
            for page, c in cold:
                if isinstance(c, dict):
                    out.append(f"| {page} | {f1(c['ms'])} | {f1(c['warm_ms'])} | {f1(c['frontier_ms'])} | {f1(c['get_tree_ms'])} | {c['block_nodes']} |")
            out.append('')
    return out


def churn_tables(run):
    out = ['## Churn (WM-4 update latency, M-13 identity)', '']
    for f in sorted(glob.glob(f'{run}/*/*.churn*.json')):
        d = load(f)
        br = f.split('/')[-2]
        out += [f"### {d['page']} / {br} / use_dom_id={d['use_dom_id']}", '',
                f"initial build {f1(d['initial_build_ms'])} ms, first block {f1(d.get('initial_first_block_ms'))} ms ({d.get('initial_first_block_nodes')} nodes), initial mirror consistent: {d['initial_consistent']}; build detail {d.get('initial_build_detail')}", '',
                '| step | events | dom->first event ms | dom->mirror ms | nodes fetched | consistent | after revalidate | ids stable | moved | gone | wrong node | resync | heuristic share |',
                '|---|---|---|---|---|---|---|---|---|---|---|---|---|']
        for r in d['steps']:
            if 'error' in r:
                out.append(f"| {r['step']} | ERROR {r['error']} |")
                continue
            out.append(f"| {r['step']} | {int(r['events'])} | {f1(r['dom_to_first_event_ms'])} | {f1(r['dom_to_last_push_ms'])} | {r['nodes_fetched']} | {r['consistent']} | {r.get('consistent_after_revalidate')} | {r['id_stable']}/{r['ids_tracked']} | {r['id_moved']} | {r['id_gone']} | {r['wrong_node']} | {r['resync']} | {r['match_rungs']['heuristic_share']:.2f} |")
        out.append('')
    return out


def scale_tables(run):
    out = ['## Scaling (M-15)', '']
    for f in sorted(glob.glob(f'{run}/*/*.scale-*.json')):
        d = load(f)
        out += [f"### {d['page']}: {d['nodes']} nodes, {d['subtree_replies']} GetTree replies, {d['reply_bytes'] / 1e6:.1f} MB, {d['cores']} cores", '',
                '| decode threads | p50 ms | speedup |', '|---|---|---|']
        for r in d['decode_scaling']:
            out.append(f"| {r['threads']} | {f1(r['decode_ms']['p50'])} | {r['speedup_vs_1']:.2f} |")
        out += ['', '| P2P connections | fetch total p50 ms |', '|---|---|']
        for r in d['connection_scaling']:
            v = r['result']['variants'][0]
            out.append(f"| {r['connections']} | {f1(v['total_ms']['p50'])} |")
        out.append('')
    return out


def research_tables(run):
    out = ['## Research (TBD-WEB-1/2/3)', '']
    for br in ('firefox', 'chromium'):
        files = sorted(glob.glob(f'{run}/{br}/*.research-{br}.json'))
        if not files:
            continue
        out += [f'### {br}', '', '| page | step | rows before->after | kept | same path | id stable | wrong node | positional reassigned | nodes persisting | state flips (showing) | with event | events |', '|---|---|---|---|---|---|---|---|---|---|---|---|']
        for f in files:
            d = load(f)
            for r in d['steps']:
                if 'error' in r:
                    continue
                ev = ', '.join(f'{k}={v}' for k, v in r['events'].items())
                out.append(f"| {d['page']} | {r['step']} | {r['rows_before']}->{r['rows_after']} | {r['rows_retained']} | {r['retained_same_path']} | {r['retained_id_stable']} | {r['wrong_node']} | {r['positional_reassigned']} | {r['nodes_persisting']} | {r['showing_or_visible_flipped']} | {r['showing_or_visible_flips_with_event']} | {ev} |")
        out += ['', 'Attribute names seen on list items, rows and cells:', '']
        for f in files:
            d = load(f)
            keys = sorted({k for a in d['attribute_samples'] for k in a['attrs']})
            out.append(f"* {d['page']}: {', '.join(keys)}")
        out.append('')
    return out


def main():
    run = sys.argv[1].rstrip('/')
    cond = os.path.join(run, 'conditions.txt')
    out = [f'# Summary of {run}', '']
    if os.path.exists(cond):
        out += ['```', open(cond).read().strip(), '```', '']
    out += build_tables(run) + churn_tables(run) + scale_tables(run) + research_tables(run)
    print('\n'.join(out))


if __name__ == '__main__':
    main()
