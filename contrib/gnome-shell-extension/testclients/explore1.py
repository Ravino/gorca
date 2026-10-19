import sys, time, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ptestlib import *
print('ping', ping(), 'caps', caps())
print('monitors', monitors())
print(probe('HideOverview'))
c = Client(['/usr/bin/python3', os.path.join(os.path.dirname(os.path.abspath(__file__)), 'gtk4_clicker.py'), 'T4', 'menu'], name='t4')
wait_for(lambda: 'READY' in c.text())
time.sleep(1.5)
for w in windows(): print({k: w[k] for k in ('id','title','type','transient-for','frame-x','frame-y','frame-w','frame-h','buffer-x','buffer-y','buffer-w','buffer-h','monitor','visible')})
print(get_pointer())
c.kill()
