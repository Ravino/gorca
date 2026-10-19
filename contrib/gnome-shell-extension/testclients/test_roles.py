#!/usr/bin/python3
# Access-control tests for helper v2: role b (allow-listed, capture only), role c (not listed), role a (first-match caps).
# Usage: test_roles.py b|c|a
import json, os, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ptestlib import *

W = os.environ['W']
role = sys.argv[1]
R = Results()
CFG = os.path.join(W, 'config', 'gorca', 'shell-helper.json')


def err(fn):
    try:
        return None, fn()
    except HelperError as e:
        return e, None


def shell_log():
    return open(os.path.join(W, 'shell.log')).read()


def rewrite(rules):
    tmp = CFG + '.tmp'
    with open(tmp, 'w') as f:
        json.dump({'allowed': rules}, f)
    os.chmod(tmp, 0o600)
    os.replace(tmp, CFG)


if role == 'b':
    R.check('B Ping works (allow-listed, caps default to capture)', ping() == 2)
    R.check('B Capabilities == [capture]', caps() == ['capture'], str(caps()))
    R.check('B GetWindows works', isinstance(windows(), list))
    for nm, fn in [('GetPointer', get_pointer), ('PointerMove', lambda: move(0, 'stage', 300, 10)),
                   ('PointerClickAt', lambda: click_at(0, 'stage', 300, 10, 1)), ('PointerButton', lambda: button(1, 'press')),
                   ]:
        e, _ = err(fn)
        R.check(f'B {nm} -> AccessDenied (no pointer cap)', e and e.name == 'AccessDenied', str(e))
    R.check('B PointerRelease is allowed for any allow-listed caller and releases nothing', err(release)[0] is None and release() == 0)
    log0 = shell_log().count('DENIED')
    for i in range(30):
        err(get_pointer)
    log1 = shell_log().count('DENIED')
    R.check('B repeated cap denials log at most once per (sender, config stamp)', log1 - log0 <= 1, f'{log1 - log0} new lines')
    # config stamp change grants the cap without reconnecting
    exe = os.path.realpath(sys.executable)
    cg = open('/proc/self/cgroup').read().strip().split('::', 1)[1]
    time.sleep(1.1)
    rewrite([{'exe': exe, 'cgroup': cg, 'caps': ['capture', 'pointer', 'bogus']}])
    e, p = err(get_pointer)
    R.check('B after the owner edits the allow-list the cap is picked up (stamp change voids the cached denial)', e is None and 'x' in p, str(e or p))
    R.check('B unknown cap string ignored with a warning', 'ignoring unknown cap "bogus"' in shell_log())
    # AL-1: first matching rule wins, later rules are not merged
    time.sleep(1.1)
    rewrite([{'exe': exe, 'cgroup': cg, 'caps': ['capture']}, {'exe': exe, 'cgroup': cg, 'caps': ['capture', 'pointer']}])
    e, _ = err(get_pointer)
    R.check('B AL-1: narrower rule first is never widened by a later one', e and e.name == 'AccessDenied', str(e))
    time.sleep(1.1)
    rewrite([{'exe': exe, 'cgroup': cg, 'caps': ['pointer']}])
    e, _ = err(lambda: call('CaptureArea', '(iiiia{sv})', (0, 0, 10, 10, {}), '((uuusd)ay)'))
    R.check('B rule without "capture" denies capture', e and e.name == 'AccessDenied', str(e))
    R.check('B rule without "capture" still allows Ping and the pointer cap', ping() == 2 and err(get_pointer)[0] is None)
elif role == 'c':
    e, _ = err(ping)
    R.check('C not allow-listed -> AccessDenied', e and e.name == 'AccessDenied', str(e))
    log0 = shell_log().count('DENIED')
    names = {}
    t0 = time.time()
    for i in range(200):
        e, _ = err(ping)
        k = 'AccessDenied' if e.name == 'AccessDenied' else e.msg.split(':')[0]
        names[k] = names.get(k, 0) + 1
    el = time.time() - t0
    log1 = shell_log().count('DENIED')
    R.check('C flood: per-sender bucket answers rate: before any /proc work', names.get('rate', 0) > 100, f'{names} in {el:.2f}s')
    R.check('C flood: DENIED logged at most once per (sender, config stamp)', log1 - log0 <= 1, f'{log1 - log0} lines')
    # the limits are global: many connections do not multiply them
    conns = [new_conn() for _ in range(10)]
    log0 = shell_log().count('DENIED')
    nd = nr = 0
    time.sleep(1.2)
    t0 = time.time()
    for c in conns:
        for i in range(30):
            try:
                call('Ping', rsig='(u)', c=c)
            except HelperError as e:
                if e.name == 'AccessDenied':
                    nd += 1
                else:
                    nr += 1
    el = time.time() - t0
    log1 = shell_log().count('DENIED')
    R.check('C 10 connections x 30 calls: each connection is bounded by its OWN bucket (<= 20 burst + 10/s each)', nd <= 10 * (20 + 10 * el) + 10 and nr > 50, f'denied={nd} rate={nr} in {el:.2f}s')
    R.check('C DENIED log lines are globally limited to ~1/s', log1 - log0 <= int(el) + 2, f'{log1 - log0} lines in {el:.2f}s')
    e, _ = err(get_pointer)
    R.check('C pointer methods AccessDenied or rate:', e is not None, str(e))
sys.exit(R.summary())
