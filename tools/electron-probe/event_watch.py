#!/usr/bin/python3 -I
"""Read-only AT-SPI event watcher for one application, PRIVACY-REDACTED.

Usage: event_watch.py APP_PATTERN SECONDS [OUTFILE]

Registers listeners (registryd RegisterEvent only; no actions, no focus changes). For each event prints:
t_ms, type, d1/d2, source role + state letters + xml-roles/tag/posinset/setsize + role chain to the document
(roles only), and for text events only lengths. Names/text are reduced to length#hash4.
"""
import sys, time, hashlib, gi
gi.require_version('Atspi', '2.0')
from gi.repository import Atspi, GLib

pat = sys.argv[1].lower()
secs = float(sys.argv[2])
out = open(sys.argv[3], 'w', buffering=1) if len(sys.argv) > 3 else sys.stdout
S = Atspi.StateType
T0 = time.monotonic()
counts = {}


def red(s):
    s = s or ''
    return '0' if not s else '%d#%s' % (len(s), hashlib.sha1(s.encode()).hexdigest()[:4])


def flags(n):
    s = n.get_state_set()
    m = [('fcsbl', S.FOCUSABLE), ('FOCUSED', S.FOCUSED), ('SELECTED', S.SELECTED), ('expd', S.EXPANDED), ('chk', S.CHECKED),
         ('edit', S.EDITABLE), ('show', S.SHOWING), ('mda', S.MANAGES_DESCENDANTS)]
    return ','.join(a for a, t in m if s.contains(t))


def desc(n):
    if n is None:
        return 'None'
    try:
        at = n.get_attributes() or {}
    except Exception:
        at = {}
    keep = ' '.join('%s=%s' % (k, at[k]) for k in ('xml-roles', 'tag', 'posinset', 'setsize', 'level', 'live') if k in at)
    try:
        par = n.get_parent()
        idx = n.get_index_in_parent()
        ok = par is not None and idx >= 0 and par.get_child_at_index(idx) == n
        reach = 'reach=%s idx=%d sib=%d' % ('ok' if ok else 'BROKEN', idx, par.get_child_count() if par else -1)
    except Exception:
        reach = 'reach=?'
    return '%s[%s] %s name=%s ch=%d %s' % (n.get_role_name(), flags(n), keep, red(n.get_name()), n.get_child_count(), reach)


def chain(n, maxn=9):
    r = []
    while n is not None and len(r) < maxn:
        rn = n.get_role_name()
        r.append(rn)
        if rn == 'document web':
            break
        n = n.get_parent()
    return '<'.join(r)


def cb(ev):
    try:
        src = ev.source
        app = src.get_application() if src else None
        if app is None or pat not in (app.get_name() or '').lower():
            return
        t = ev.type
        counts[t] = counts.get(t, 0) + 1
        if t.startswith('object:text-changed') or t.startswith('object:property-change'):
            line = '%7d %s d1=%d d2=%d src=%s' % ((time.monotonic() - T0) * 1000, t, ev.detail1, ev.detail2, desc(src))
        else:
            ad = ev.any_data
            adr = ''
            if t.startswith('object:children-changed') and ad is not None:
                try:
                    adr = ' any=' + ad.get_role_name()
                except Exception:
                    adr = ' any=?'
            line = '%7d %s d1=%d d2=%d src=%s chain=%s%s' % ((time.monotonic() - T0) * 1000, t, ev.detail1, ev.detail2,
                                                                 desc(src), chain(src), adr)
        out.write(line + '\n')
    except Exception as e:
        out.write('ERR %s\n' % e)


L = Atspi.EventListener.new(cb)
for t in ('object:state-changed:focused', 'object:state-changed:selected', 'object:state-changed:expanded',
          'object:state-changed:showing', 'object:text-caret-moved', 'object:text-selection-changed',
          'object:selection-changed', 'object:active-descendant-changed', 'object:children-changed:add',
          'object:children-changed:remove', 'object:text-changed:insert', 'object:text-changed:delete',
          'window:activate', 'window:deactivate', 'object:announcement', 'document:load-complete'):
    L.register(t)
loop = GLib.MainLoop()
GLib.timeout_add(int(secs * 1000), loop.quit)
loop.run()
out.write('SUMMARY ' + repr(counts) + '\n')
