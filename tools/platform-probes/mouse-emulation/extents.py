#!/usr/bin/python3 -I
# extents.py A11Y_ADDR TITLE : find the button named Target in the app whose frame title is TITLE; print SCREEN/WINDOW/PARENT extents.
import sys, gi
gi.require_version('Atspi','2.0')
from gi.repository import Atspi
Atspi.init()
def walk(a, depth=0):
    yield a
    for i in range(a.get_child_count()):
        c = a.get_child_at_index(i)
        if c: yield from walk(c, depth+1)
desk = Atspi.get_desktop(0)
for app in [desk.get_child_at_index(i) for i in range(desk.get_child_count())]:
    if not app: continue
    for n in walk(app):
        try: nm = n.get_name(); role = n.get_role_name()
        except Exception: continue
        if role in ('frame','window') and nm == sys.argv[1]:
            c = n.get_component_iface()
            for t, nmct in ((Atspi.CoordType.SCREEN,'SCREEN'),(Atspi.CoordType.WINDOW,'WINDOW')):
                r = c.get_extents(t); print('FRAME', nmct, r.x, r.y, r.width, r.height)
        if role in ('push button','button') and nm == 'Target':
            c = n.get_component_iface()
            for t, nmct in ((Atspi.CoordType.SCREEN,'SCREEN'),(Atspi.CoordType.WINDOW,'WINDOW'),(Atspi.CoordType.PARENT,'PARENT')):
                r = c.get_extents(t); print('BUTTON', nmct, r.x, r.y, r.width, r.height)
