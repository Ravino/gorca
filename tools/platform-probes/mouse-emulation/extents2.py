#!/usr/bin/python3 -I
# extents2.py : print SCREEN/WINDOW extents of every button/link named Target plus the frame/document extents, per app.
import sys, gi
gi.require_version('Atspi','2.0')
from gi.repository import Atspi
Atspi.init()
def walk(a, d=0):
    yield a
    if d > 40: return
    for i in range(a.get_child_count()):
        c = a.get_child_at_index(i)
        if c: yield from walk(c, d+1)
desk = Atspi.get_desktop(0)
for i in range(desk.get_child_count()):
    app = desk.get_child_at_index(i)
    if not app: continue
    an = app.get_name()
    if sys.argv[1] not in (an or ''): continue
    for n in walk(app):
        try: nm, role = n.get_name(), n.get_role_name()
        except Exception: continue
        if role in ('frame','window','document web','document frame') or nm == 'Target':
            c = n.get_component_iface()
            if not c: continue
            out = []
            for t, k in ((Atspi.CoordType.SCREEN,'S'),(Atspi.CoordType.WINDOW,'W')):
                r = c.get_extents(t); out.append('%s=%d,%d %dx%d' % (k, r.x, r.y, r.width, r.height))
            print(an, '|', role, repr(nm), '|', '  '.join(out))
