"""pyatspi (libatspi) client tests. Kept in a separate process from test_smoke.py because libatspi
aborts the whole process on some malformed payloads (see Malformed.test_libatspi_client_vs_bad_payload)."""
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import pyatspi  # noqa: E402
from fakeapp_ctl import FakeApp  # noqa: E402


class PyatspiTree(unittest.TestCase):
    def test_tree_enumeration_pyatspi(self):
        with FakeApp.start("--scenario", "form", "--name", "formapp") as app:
            desktop = pyatspi.Registry.getDesktop(0)
            found = [a for a in desktop if a.name == "formapp"]
            self.assertEqual(len(found), 1)
            frame = found[0][0]
            self.assertEqual(frame.name, "Form")
            self.assertEqual(frame.getRole(), pyatspi.ROLE_FRAME)
            panel = frame[0]
            self.assertEqual([c.name for c in panel],
                             ["Name:", "name", "Email:", "email", "Subscribe", "Submit", "Cancel", "notes"])
            self.assertEqual(panel[1].getRole(), pyatspi.ROLE_ENTRY)
            self.assertEqual(panel[5].getRole(), pyatspi.ROLE_PUSH_BUTTON)
            self.assertEqual(int(pyatspi.ROLE_TERMINAL), 60)       # numbers hardcoded in fakeapp.py
            self.assertEqual(int(pyatspi.ROLE_APPLICATION), 75)
            self.assertEqual(int(pyatspi.STATE_FOCUSED), 12)

    def test_small_tree_via_pyatspi(self):
        with FakeApp.start("--scenario", "tree", "--nodes", "10", "--fanout", "3", "--name", "t10"):
            app = [a for a in pyatspi.Registry.getDesktop(0) if a.name == "t10"][0]
            count = 0
            stack = [app]
            while stack:
                n = stack.pop()
                count += 1
                stack.extend(list(n))
            self.assertEqual(count, 10)



if __name__ == "__main__":
    unittest.main(verbosity=2)
