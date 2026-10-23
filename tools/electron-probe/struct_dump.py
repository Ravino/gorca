#!/usr/bin/python3 -I
"""Read-only structural dump of an application's AT-SPI tree with PRIVACY redaction.

Usage: struct_dump.py APP_PATTERN [max_depth] [max_nodes] [max_children_per_node]

Prints per node: role, state letters, interface letters, attribute KEYS (xml-roles, tag, live
values are allowed; every other value is reduced to its length), relation types, child count,
name/description/text as length + 4-hex hash only. No actions, no focus changes, no screen capture.
"""
import sys, hashlib, gi
gi.require_version('Atspi', '2.0')
from gi.repository import Atspi

pat = sys.argv[1].lower()
maxd = int(sys.argv[2]) if len(sys.argv) > 2 else 12
maxn = int(sys.argv[3]) if len(sys.argv) > 3 else 3000
maxc = int(sys.argv[4]) if len(sys.argv) > 4 else 6
S = Atspi.StateType
STATES = ('EDITABLE', 'FOCUSABLE', 'FOCUSED', 'SHOWING', 'VISIBLE', 'SELECTABLE', 'SELECTED', 'EXPANDABLE', 'EXPANDED',
          'CHECKED', 'MULTI_LINE', 'SINGLE_LINE', 'ACTIVE', 'ENABLED', 'SENSITIVE', 'FOCUSABLE', 'MANAGES_DESCENDANTS',
          'MULTISELECTABLE', 'BUSY', 'INDETERMINATE', 'HORIZONTAL', 'VERTICAL', 'READ_ONLY')
ABBR = {'FOCUSABLE': 'fcsbl', 'FOCUSED': 'FOCUSED', 'SELECTABLE': 'selbl', 'SELECTED': 'SELECTED', 'EXPANDABLE': 'expbl', 'EXPANDED': 'EXPANDED',
        'ENABLED': 'en', 'SENSITIVE': 'sens', 'SHOWING': 'show', 'VISIBLE': 'vis'}
ALLOW_VALUES = {'xml-roles', 'tag', 'live', 'container-live', 'atomic', 'relevant', 'level', 'posinset', 'setsize',
                'tabindex', 'hidden', 'busy', 'orientation', 'haspopup', 'roledescription', 'multiselectable'}


def red(s):
    s = s or ''
    if not s:
        return '0'
    return '%d#%s' % (len(s), hashlib.sha1(s.encode()).hexdigest()[:4])


def st(n):
    s = n.get_state_set()
    out = []
    for nm in STATES:
        t = getattr(S, nm, None)
        if t is not None and s.contains(t):
            out.append(ABBR.get(nm, nm[:3]))
    return ','.join(out)


def ifc(n):
    try:
        return ','.join(x[:4] for x in n.get_interfaces())
    except Exception:
        return '?'


def rels(n):
    try:
        rs = n.get_relation_set()
        return ','.join('%s:%d' % (r.get_relation_type().value_nick, r.get_n_targets()) for r in rs)
    except Exception:
        return ''


desk = Atspi.get_desktop(0)
apps = [desk.get_child_at_index(i) for i in range(desk.get_child_count())]
apps = [a for a in apps if a and pat in (a.get_name() or '').lower()]
print('apps:', [(a.get_name(), a.get_toolkit_name()) for a in apps])
cnt = [0]


def walk(n, d):
    if cnt[0] >= maxn or d > maxd:
        return
    cnt[0] += 1
    try:
        attrs = n.get_attributes() or {}
    except Exception:
        attrs = {}
    ak = []
    for k, v in sorted(attrs.items()):
        if k.startswith('container-') or k in ('class', 'display', 'text-align', 'name-from', 'atomic'):
            continue
        ak.append('%s=%s' % (k, v) if k in ALLOW_VALUES else '%s(%d)' % (k, len(v)))
    nc = n.get_child_count()
    print('%s%s|%s|%s|%s|name=%s|ch=%d|%s' % ('  ' * d, n.get_role_name(), ''.join(x[0] for x in ifc(n).split(',')), st(n), rels(n), red(n.get_name()), nc,
                                              ' '.join(ak)))
    seen = {}
    skipped = 0
    for i in range(nc):
        c = n.get_child_at_index(i)
        if not c:
            continue
        r = c.get_role_name()
        seen[r] = seen.get(r, 0) + 1
        focused = c.get_state_set().contains(S.FOCUSED)
        if seen[r] > maxc and not focused:
            skipped += 1
            continue
        walk(c, d + 1)
    if skipped:
        print('%s... +%d same-role siblings elided' % ('  ' * (d + 1), skipped))


for a in apps:
    walk(a, 0)
print('nodes printed', cnt[0])
