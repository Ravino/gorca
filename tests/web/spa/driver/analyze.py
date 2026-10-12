"""Event-stream summaries, identity-survival metrics and compact tree dump format."""
import collections

def ekey(e):
    # e = (t, sender, path, iface, member, kind, d1, d2, data, ref)
    return f'{e[3]}.{e[4]}' + (f':{e[5]}' if e[5] else '')

def step_summary(evs, t0, A, B):
    pa = {n['path']: n for n in A}; pb = {n['path']: n for n in B}
    def who(p):
        n = pb.get(p) or pa.get(p)
        return None if not n else f"{n['role']}" + (f"#{n['attrs']['id']}" if 'id' in n['attrs'] else '') + (f"[{n['name'][:24]}]" if n['name'] else '')
    counts = collections.Counter(ekey(e) for e in evs)
    ts = [(e[0] - t0) * 1000 for e in evs]
    buckets = collections.Counter(int(t // 100) for t in ts)
    out = {'n_events': len(evs), 'counts': dict(counts.most_common()),
           't_first_ms': round(ts[0], 1) if ts else None, 't_last_ms': round(ts[-1], 1) if ts else None,
           'max_events_per_100ms': max(buckets.values()) if buckets else 0, 'active_100ms_buckets': len(buckets),
           'distinct_sender_paths': len({e[2] for e in evs})}
    out['order'] = [[round(t, 1), ekey(e), e[2].rsplit('/', 1)[-1], e[6], e[7], who(e[2]), (e[9] or '').rsplit('/', 1)[-1] or None]
                    for t, e in list(zip(ts, evs))[:40]]
    foc = [e for e in evs if e[4] == 'StateChanged' and e[5] == 'focused' and e[6] == 1]
    out['atspi_focus'] = [who(e[2]) for e in foc[:6]]
    # ordering signature: run-length of event kinds in arrival order (first 60)
    sig = []
    for e in evs[:200]:
        k = ekey(e)
        if sig and sig[-1][0] == k: sig[-1][1] += 1
        else: sig.append([k, 1])
    out['kind_runs'] = sig[:30]
    return out

def identity(A, B):
    pa = {n['path']: n for n in A}; pb = {n['path']: n for n in B}
    sig = lambda n: (n['role'], n['name'], n['attrs'].get('id'))
    common = pa.keys() & pb.keys()
    same = inplace = recycled = 0
    for p in common:
        a, b = pa[p], pb[p]
        if sig(a) == sig(b): same += 1
        elif a['role'] == b['role'] and a['attrs'].get('id') == b['attrs'].get('id'): inplace += 1     # same node, name/text changed
        else: recycled += 1                                                                          # path now describes a different node
    ida = {n['attrs']['id']: n['path'] for n in A if 'id' in n['attrs']}; idb = {n['attrs']['id']: n['path'] for n in B if 'id' in n['attrs']}
    both = ida.keys() & idb.keys()
    kept = sum(ida[i] == idb[i] for i in both)
    # name-based matching for nodes without a DOM id: unique (role,name) in both snapshots
    def uniq(S):
        c = collections.Counter((n['role'], n['name']) for n in S if 'id' not in n['attrs'] and n['name'])
        return {(n['role'], n['name']): n['path'] for n in S if 'id' not in n['attrs'] and n['name'] and c[(n['role'], n['name'])] == 1}
    ua, ub = uniq(A), uniq(B); nb = ua.keys() & ub.keys()
    nkept = sum(ua[k] == ub[k] for k in nb)
    # silent changes: child order changed among surviving children (no add/remove), and attribute changes on surviving objects
    def kids(S):
        d = collections.defaultdict(list)
        for n in S:
            if n['p'] >= 0: d[S[n['p']]['path']].append(n['path'])
        return d
    ka, kb = kids(A), kids(B); reordered = moved_children = 0
    for par in ka.keys() & kb.keys():
        sb = set(kb[par]); sa = set(ka[par]); la = [x for x in ka[par] if x in sb]; lb = [x for x in kb[par] if x in sa]
        if la != lb:
            reordered += 1; moved_children += sum(1 for x, y in zip(la, lb) if x != y)
    reparented = sum(1 for p in common if A[pa[p]['i']]['p'] >= 0 and B[pb[p]['i']]['p'] >= 0 and A[A[pa[p]['i']]['p']]['path'] != B[B[pb[p]['i']]['p']]['path'])
    attr_changed = sum(1 for p in common if {k: v for k, v in pa[p]['attrs'].items() if k in ('id', 'class', 'xml-roles', 'level', 'live')} != {k: v for k, v in pb[p]['attrs'].items() if k in ('id', 'class', 'xml-roles', 'level', 'live')})
    state_changed = sum(1 for p in common if set(pa[p]['states']) - {'focused', 'active'} != set(pb[p]['states']) - {'focused', 'active'})
    return {'reordered_parents': reordered, 'children_moved': moved_children, 'reparented': reparented, 'attr_changed': attr_changed, 'state_changed': state_changed,
            'paths_before': len(pa), 'paths_after': len(pb), 'common_paths': len(common), 'same_sig': same, 'inplace_update': inplace,
            'path_recycled_other_node': recycled, 'gone': len(pa.keys() - pb.keys()), 'new': len(pb.keys() - pa.keys()),
            'dom_id_both': len(both), 'dom_id_path_kept': kept, 'dom_id_path_changed': len(both) - kept,
            'dom_id_only_before': len(ida.keys() - idb.keys()), 'dom_id_only_after': len(idb.keys() - ida.keys()),
            'name_match_both': len(nb), 'name_match_path_kept': nkept}

def compact_tree(nodes, meta):
    """Flat DFS arrays. Per node: [parent, role, name_len, [states], [x,y,w,h]|null, {attrs}|null, n_children, path_tail]."""
    keep = {'id', 'tag', 'xml-roles', 'level', 'display', 'class', 'name-from', 'live', 'container-live', 'relevant', 'atomic', 'busy', 'explicit-name'}
    out = []
    for n in nodes:
        at = {k: v for k, v in n['attrs'].items() if k in keep}
        out.append([n['p'], n['role'], n['nl'], [s for s in n['states'] if s not in ('enabled', 'sensitive')], n['b'], at or None, n['nc'], n['path'].rsplit('/', 1)[-1]])
    meta = dict(meta, n=len(out), fields=['parent', 'role', 'name_len', 'states', 'bounds', 'attrs', 'n_children', 'path_tail'])
    return {'meta': meta, 'nodes': out}
