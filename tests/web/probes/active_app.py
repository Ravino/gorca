#!/usr/bin/python3
"""Read-only: print which app has an ACTIVE top-level frame on the REAL a11y bus (focus-steal check)."""
import gi
gi.require_version("Atspi", "2.0")
from gi.repository import Atspi
d = Atspi.get_desktop(0); out = []
for i in range(d.get_child_count()):
    a = d.get_child_at_index(i)
    if a is None: continue
    for j in range(a.get_child_count()):
        f = a.get_child_at_index(j)
        if f is not None and f.get_state_set().contains(Atspi.StateType.ACTIVE):
            out.append(a.get_name())
print(",".join(out) or "none")
