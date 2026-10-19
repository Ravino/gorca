#!/usr/bin/python3
# Functional tests of org.gorca.ShellHelper v2 pointer methods in the PRIVATE headless shell.
# Run through test-pointer.sh (it starts the rig and runs this as the allow-listed caller A).
# Usage: test_pointer.py [--slow]    --slow adds the 125 s backstop test.
import os, signal, sys, threading, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ptestlib import *

HERE = os.path.dirname(os.path.abspath(__file__))
W = os.environ['W']
R = Results()
SLOW = '--slow' in sys.argv
clients = []


def spawn(title, mode=None, x=None, y=None, script='gtk4_clicker.py'):
    argv = ['/usr/bin/python3', os.path.join(HERE, script), title] + ([mode] if mode else [])
    c = Client(argv, name=title.replace(' ', '_'))
    clients.append(c)
    assert wait_for(lambda: 'READY' in c.text(), 20), f'{title} did not start: {c.text()}'
    w = wait_for(lambda: (find_windows(title=title) or [None])[0], 10)
    assert w, f'no window {title}'
    if x is not None:
        time.sleep(1.5)
        probe('MoveFrame', w['id'], x, y)
        w = wait_for(lambda: (lambda ws: ws and ws[0]['frame-x'] == x and ws[0]['frame-y'] == y and ws[0])(find_windows(title=title)), 5) or w
    return c, w


def rewin(title):
    return find_windows(title=title)[0]


def evs(c):
    return c.events()


def wait_events(c, n, timeout=3.0):
    return wait_for(lambda: len(evs(c)) >= n, timeout)


def near(a, b, tol=1.01):
    return abs(float(a) - float(b)) <= tol


def err(fn):
    try:
        r = fn()
        return None, r
    except HelperError as e:
        return e, None


def main():
    probe('HideOverview')
    time.sleep(0.6)
    # ---- T1 basics
    R.check('T1 ping == 2', ping() == 2, str(ping()))
    R.check('T1 capabilities', caps() == ['capture', 'pointer'], str(caps()))
    ms = monitors()
    R.check('T1 monitors carry x-scale', all('x-scale' in m for m in ms), str(ms[0]))
    A, wa = spawn('PA', None, 100, 100)
    R.check('T1 windows carry transient-for', 'transient-for' in wa and wa['transient-for'] == 0, str(wa.get('transient-for')))

    # ---- T4 validation
    t = lambda **k: opts(**k)
    for nm, fn in [
        ('button 0', lambda: click_at(wa['id'], 'frame', 10, 10, 0)),
        ('button 4', lambda: click_at(wa['id'], 'frame', 10, 10, 4)),
        ('no not-after', lambda: click_at(wa['id'], 'frame', 10, 10, 1, opts(not_after_ms=None))),
        ('NaN x', lambda: click_at(wa['id'], 'frame', float('nan'), 10, 1)),
        ('huge x', lambda: click_at(wa['id'], 'frame', 1e9, 10, 1)),
        ('bad origin', lambda: click_at(wa['id'], 'screen', 10, 10, 1)),
        ('frame without window', lambda: click_at(0, 'frame', 10, 10, 1)),
        ('count 3', lambda: click_at(wa['id'], 'frame', 10, 10, 1, opts(count=V('u', 3)))),
        ('bad if-occluded', lambda: click_at(wa['id'], 'frame', 10, 10, 1, opts(**{'if-occluded': V('s', 'x')}))),
        ('move no not-after', lambda: move(0, 'stage', 5, 5, opts(not_after_ms=None))),
        ('button bad action', lambda: button(1, 'tap')),
        ('button no not-after', lambda: button(1, 'click', opts(not_after_ms=None))),
    ]:
        e, _ = err(fn)
        R.check(f'T4 {nm} -> Failed bad-args', e and e.name == 'Failed' and e.msg.startswith('bad-args:'), str(e))
    e, _ = err(lambda: click_at(987654321, 'frame', 1, 1, 1))
    R.check('T4 unknown window -> no-window:', e and e.msg.startswith('no-window:'), str(e))
    R.check('T4 nothing injected by rejected calls', len(evs(A)) == 0, str(evs(A)))
    time.sleep(1.2)   # let the pointer bucket refill after the burst above

    # ---- T5 expiry
    n0 = len(evs(A))
    r = click_at(wa['id'], 'frame', 200, 200, 1, opts(not_after_ms=-5))
    R.check('T5 past not-after -> expired', r['outcome'] == 'expired', str(r))
    r = move(0, 'stage', 40, 40, opts(not_after_ms=-5))
    R.check('T5 move past not-after -> expired', r['outcome'] == 'expired', str(r))
    r = button(1, 'press', opts(not_after_ms=-5))
    R.check('T5 button past not-after -> expired, nothing held', r['outcome'] == 'expired' and r['held'] == 0, str(r))
    time.sleep(0.4)
    R.check('T5 expired calls inject nothing', len(evs(A)) == n0, str(evs(A)))

    # ---- T5b a release never expires (F-3), GetPointer shares the pointer bucket
    wa = rewin('PA')
    move(wa['id'], 'frame', 200, 200)
    n0 = len(evs(A))
    button(1, 'press')
    r = button(1, 'release', opts(not_after_ms=-50))
    ok = wait_for(lambda: any(x[0] == 'release' for x in evs(A)[n0:]), 2)
    R.check('T5b expired release still releases', r['outcome'] == 'done' and r['held'] == 0 and ok, f'{r} {evs(A)[n0:]}')
    time.sleep(2.2)
    okg = failg = 0
    for i in range(80):
        try:
            get_pointer(); okg += 1
        except HelperError as e:
            assert e.msg.startswith('rate:'), e
            failg += 1
    R.check('T5b GetPointer is on the pointer bucket (burst ~40 then rate:)', 38 <= okg <= 50 and failg >= 30, f'ok={okg} limited={failg}')
    time.sleep(2.2)

    # ---- T6 click at frame origin, all buttons, double click
    for b in (1, 2, 3):
        n0 = len(evs(A))
        r = click_at(wa['id'], 'frame', 120.0, 150.0, b)
        ok = wait_events(A, n0 + 2)
        e = evs(A)[n0:n0 + 2]
        R.check(f'T6 click b{b} at frame (120,150)',
                ok and r['outcome'] == 'done' and e[0][0] == 'press' and e[0][1] == str(b) and near(e[0][2], 120) and near(e[0][3], 150)
                and e[1][0] == 'release', f'{r} {e}')
    n0 = len(evs(A))
    t0 = time.time()
    r = click_at(wa['id'], 'frame', 130, 140, 1, opts(count=V('u', 2)))
    dt = (time.time() - t0) * 1000
    ok = wait_events(A, n0 + 4)
    R.check('T6 count=2 -> 4 events, reply after the second click', ok and r['outcome'] == 'done' and dt >= 25,
            f'{dt:.0f} ms {evs(A)[n0:]}')
    # buffer origin: GTK4 buffer rect includes the CSD shadow
    wa = rewin('PA')
    n0 = len(evs(A))
    bx, by = wa['buffer-x'] - wa['frame-x'], wa['buffer-y'] - wa['frame-y']
    r = click_at(wa['id'], 'buffer', 60 - bx, 70 - by, 1)
    ok = wait_events(A, n0 + 2)
    R.check('T6 buffer origin resolves to the same point', ok and r['outcome'] == 'done' and near(evs(A)[n0][2], 60) and near(evs(A)[n0][3], 70),
            f'{r} {evs(A)[n0:n0 + 1]} buffer-frame=({bx},{by})')

    # ---- T7 move + GetPointer
    r = move(wa['id'], 'frame', 210, 220)
    time.sleep(0.3)
    p = get_pointer()
    R.check('T7 move reply', r['outcome'] == 'done' and near(r['x'], wa['frame-x'] + 210) and near(r['y'], wa['frame-y'] + 220), str(r))
    R.check('T7 GetPointer = moved point + topmost window + window-relative',
            near(p['x'], wa['frame-x'] + 210) and near(p['y'], wa['frame-y'] + 220) and p['window'] == wa['id']
            and near(p['wx'], 210) and near(p['wy'], 220), str(p))
    r = move(0, 'stage', 300, 10)
    time.sleep(0.3)
    p = get_pointer()
    R.check('T7 stage point over Shell UI -> window 0', p['window'] == 0 and near(p['x'], 300), str(p))
    r = move(0, 'stage', 5000, 5000)
    R.check('T7 off-stage point reports clamped', r['clamped'] is True and r['outcome'] == 'done', str(r))

    # ---- T8 window guard
    n0 = len(evs(A))
    r = click_at(wa['id'], 'frame', 700, 100, 1)
    R.check('T8 frame point outside the window -> refused-outside', r['outcome'] == 'refused-outside', str(r))
    r = click_at(wa['id'], 'stage', 1100, 800, 1)
    R.check('T8 stage point outside the window -> refused-outside', r['outcome'] == 'refused-outside', str(r))
    time.sleep(0.3)
    R.check('T8 nothing injected', len(evs(A)) == n0, str(evs(A)[n0:]))

    # ---- T9 occlusion (window over window)
    B, wb = spawn('PB', None, 200, 200)    # overlaps PA (PA 100..600 x 100..500, PB 200..700 x 200..600)
    wa = rewin('PA')
    n0a, n0b = len(evs(A)), len(evs(B))
    r = click_at(wa['id'], 'frame', 300, 300, 1)     # stage (400,400): covered by PB
    R.check('T9 covered point -> refused-occluded, window-at = PB',
            r['outcome'] == 'refused-occluded' and r['occluded'] and r['window-at'] == wb['id'], str(r))
    time.sleep(0.3)
    R.check('T9 refusal injects nothing', len(evs(A)) == n0a and len(evs(B)) == n0b)
    r = click_at(wa['id'], 'frame', 300, 300, 1, opts(**{'if-occluded': V('s', 'proceed')}))
    ok = wait_events(B, n0b + 2)
    R.check('T9 if-occluded=proceed clicks whatever covers the point (PB)', r['outcome'] == 'done' and ok and len(evs(A)) == n0a, f'{r} B={evs(B)[n0b:]}')
    r = click_at(wa['id'], 'frame', 20, 20, 1)       # uncovered corner of PA
    R.check('T9 uncovered point of the lower window is clickable', r['outcome'] == 'done' and not r['occluded'], str(r))
    B.kill()
    time.sleep(0.5)

    # ---- T9b occlusion: Overview and a notification banner
    probe('ShowOverview')
    time.sleep(1.2)
    wa = rewin('PA')
    r = click_at(wa['id'], 'frame', 150, 150, 1)
    R.check('T9b Overview over the window -> refused-occluded', r['outcome'] == 'refused-occluded', str(r))
    probe('HideOverview')
    time.sleep(1.0)
    r = click_at(wa['id'], 'frame', 150, 150, 1)
    R.check('T9b after Overview hides the point is clickable again', r['outcome'] == 'done', str(r))
    mon = [m for m in monitors() if m['primary']][0]
    probe('MoveFrame', wa['id'], mon['x'] + mon['w'] // 2 - 250, 0)    # window top under the banner
    time.sleep(0.5)
    probe('Notify', 'banner')
    time.sleep(1.2)
    wa = rewin('PA')
    # banner is a Shell actor near the top centre
    r = click_at(wa['id'], 'frame', 250, 40, 1)
    R.check('T9b notification banner over the window -> refused-occluded', r['outcome'] == 'refused-occluded' and r['window-at'] == 0, str(r))
    probe('MoveFrame', wa['id'], 100, 100)
    time.sleep(0.8)
    wa = rewin('PA')

    # ---- T9c occlusion by a transient dialog; T9d popup family is NOT an occluder
    D, wd = spawn('PD', 'dialog', 700, 100)
    time.sleep(1.0)
    wd = rewin('PD')
    dlg = [x for x in windows() if x['title'] == 'PD dialog']
    R.check('T9c dialog is a non-popup transient of its parent', dlg and dlg[0]['transient-for'] == wd['id'] and dlg[0]['type'] in (3, 4, 0), str(dlg and (dlg[0]['type'], dlg[0]['transient-for'])))
    if dlg:
        probe('MoveFrame', dlg[0]['id'], wd['frame-x'] + 130, wd['frame-y'] + 100)
        time.sleep(1.0)
        dlg = [x for x in windows() if x['title'] == 'PD dialog']
        dx, dy = dlg[0]['frame-x'] + dlg[0]['frame-w'] // 2, dlg[0]['frame-y'] + dlg[0]['frame-h'] // 2
        r = click_at(wd['id'], 'stage', dx, dy, 1)
        R.check('T9c attached dialog over the parent -> refused-occluded (dialogs are occluders, C-2)',
                r['outcome'] == 'refused-occluded' and r['window-at'] == dlg[0]['id'], str(r))
        r = click_at(dlg[0]['id'], 'stage', dx, dy, 1)
        R.check('T9c the dialog itself is clickable', r['outcome'] == 'done', str(r))
    D.kill()
    time.sleep(0.5)
    M, wm = spawn('PM', 'menu', 700, 300)
    time.sleep(1.0)
    before = {x['id'] for x in windows()}
    click_at(wm['id'], 'frame', 41, 56, 1)       # MenuButton below the 39 px default titlebar
    time.sleep(1.2)
    pops = [x for x in windows() if x['id'] not in before]
    R.check('T9d popover is listed as a type-9 popup transient of its parent', pops and pops[0]['type'] == 9 and pops[0]['transient-for'] == wm['id'], str(pops and (pops[0]['type'], pops[0]['transient-for'])))
    if pops:
        px, py = pops[0]['frame-x'] + 54, pops[0]['frame-y'] + 40
        n0 = len(evs(M)); t0 = M.text().count('EV activate')
        r = click_at(wm['id'], 'stage', px, py, 1)
        time.sleep(0.8)
        R.check('T9d point inside the popup passes guard and occlusion with window = parent (family) and activates the item',
                r['outcome'] == 'done' and not r['occluded'] and r['window-at'] == pops[0]['id'] and M.text().count('EV activate') > t0, str(r))
    M.kill()
    time.sleep(0.5)

    # ---- T10 stage guard (window = 0)
    n0 = len(evs(A))
    r = click_at(0, 'stage', wa['frame-x'] + 150, wa['frame-y'] + 150, 1)
    R.check('T10 stage click inside an app window -> refused-outside', r['outcome'] == 'refused-outside', str(r))
    r = click_at(0, 'stage', 300, 10, 1)
    R.check('T10 stage click on Shell UI (top bar, empty part) -> done', r['outcome'] == 'done' and r['window-at'] == 0, str(r))
    time.sleep(0.3)
    R.check('T10 only the allowed click was injected, none in the window', len(evs(A)) == n0)

    # ---- T11 held buttons: press/release, drag, single holder, release on vanish
    wa = rewin('PA')
    p1 = move(wa['id'], 'frame', 100, 100)
    R.check('T11 setup move', p1['outcome'] == 'done', f'{p1} frame=({wa["frame-x"]},{wa["frame-y"]})')
    n0 = len(evs(A))
    r = button(1, 'press')
    R.check('T11 press -> held bit 1', r['held'] == 1, str(r))
    r = button(1, 'press')
    R.check('T11 second press of the held button is a no-op reporting held', r['held'] == 1 and len(evs(A)) <= n0 + 1, str(r))
    move(wa['id'], 'frame', 180, 190)
    c2 = new_conn()
    e, _ = err(lambda: button(3, 'press', c=c2))
    R.check('T11 press from another caller while held -> busy:', e and e.msg.startswith('busy:'), str(e))
    e, _ = err(lambda: click_at(wa['id'], 'frame', 50, 50, 3, c=c2))
    R.check('T11 click from another caller while held -> busy:', e and e.msg.startswith('busy:'), str(e))
    r = button(1, 'release', c=c2)
    R.check('T11 release from a non-holder releases nothing', r['held'] == 0 and not any(x[0] == 'release' for x in evs(A)[n0:]), str(r))
    R.check('T11 PointerRelease from a non-holder returns 0', release(c=c2) == 0)
    r = button(1, 'release')
    ok = wait_for(lambda: any(x[0] == 'release' for x in evs(A)[n0:]), 2)
    e = evs(A)[n0:]
    R.check('T11 drag: press at (100,100) ... release at (180,190), held cleared',
            ok and r['held'] == 0 and e[0][0] == 'press' and near(e[0][2], 100) and e[-1][0] == 'release' and near(e[-1][2], 180) and near(e[-1][3], 190), f'{r} {e}')
    c2.close_sync(None)
    # holder vanishes with a button down
    c3 = new_conn()
    n0 = len(evs(A))
    button(1, 'press', c=c3)
    time.sleep(0.2)
    t0 = time.time()
    c3.close_sync(None)
    ok = wait_for(lambda: any(x[0] == 'release' for x in evs(A)[n0:]), 3)
    R.check('T11 holder vanishes -> button released <= 1 s', ok and time.time() - t0 < 1.0, f'{(time.time() - t0) * 1000:.0f} ms')
    # PointerRelease releases everything held
    n0 = len(evs(A))
    move(wa['id'], 'frame', 300, 250)
    button(1, 'press'); button(3, 'press')
    r = release()
    R.check('T11 PointerRelease releases both buttons (reply 2, none held after)', r == 2 and button(2, 'release')['held'] == 0, str(r))
    # click of a held button releases it
    n0 = len(evs(A))
    button(1, 'press'); r = button(1, 'click')
    R.check('T11 click of a held button just releases it', r['held'] == 0 and wait_for(lambda: any(x[0] == 'release' for x in evs(A)[n0:]), 2), str(r))

    # ---- T12 rate limit: 20/s, burst 40; count=2 costs 2
    time.sleep(2.2)
    ok = fail = 0
    for i in range(80):
        try:
            move(0, 'stage', 300 + i % 5, 10)
            ok += 1
        except HelperError as e:
            assert e.msg.startswith('rate:'), e
            fail += 1
    R.check('T12 80 rapid moves: burst ~40 then rate: errors', 38 <= ok <= 50 and fail >= 30, f'ok={ok} rate-limited={fail}')
    time.sleep(1.1)
    ok2 = 0
    for i in range(40):
        try:
            move(0, 'stage', 300, 11)
            ok2 += 1
        except HelperError:
            pass
    R.check('T12 after ~1 s about 20 tokens are back', 15 <= ok2 <= 30, f'ok={ok2}')
    time.sleep(2.5)
    ok3 = 0
    tt = time.time()
    for i in range(60):
        try:
            click_at(0, 'stage', 300, 10, 1, opts(count=V('u', 2)))
            ok3 += 1
        except HelperError:
            break
    el = time.time() - tt
    exp = (40 + 20 * el) / 2
    R.check('T12 count=2 clicks cost 2 each: ok ~ (40 + 20/s * elapsed) / 2', abs(ok3 - exp) <= 3, f'ok={ok3} expected~{exp:.1f} elapsed={el:.2f}s')
    time.sleep(2.5)

    # ---- T13 left-handed swap (S-5)
    gs = lambda v: subprocess.run(['gsettings', 'set', 'org.gnome.desktop.peripherals.mouse', 'left-handed', v],
                                  env=dict(os.environ, XDG_CONFIG_HOME=os.path.join(W, 'config'), GSETTINGS_BACKEND='keyfile'),
                                  capture_output=True, text=True)
    out = gs('true')
    time.sleep(1.0)
    n0 = len(evs(A))
    click_at(wa['id'], 'frame', 150, 150, 1)
    wait_events(A, n0 + 2)
    got_logical = evs(A)[n0][1] if len(evs(A)) > n0 else None
    n0 = len(evs(A))
    click_at(wa['id'], 'frame', 150, 150, 3)
    wait_events(A, n0 + 2)
    got_logical_r = evs(A)[n0][1] if len(evs(A)) > n0 else None
    n0 = len(evs(A))
    click_at(wa['id'], 'frame', 150, 150, 1, opts(logical=V('b', False)))
    wait_events(A, n0 + 2)
    got_phys = evs(A)[n0][1] if len(evs(A)) > n0 else None
    R.check('T13 left-handed: logical left (1) is delivered as button 3, logical right as 1, logical=false unswapped',
            (got_logical, got_logical_r, got_phys) == ('3', '1', '1'), f'{got_logical},{got_logical_r},{got_phys} {out.stderr.strip()}')
    # a held lock keeps its physical button even if the setting flips meanwhile
    n0 = len(evs(A))
    button(1, 'press'); gs('false'); time.sleep(0.8); button(1, 'release')
    wait_for(lambda: any(x[0] == 'release' for x in evs(A)[n0:]), 2)
    e = evs(A)[n0:]
    R.check('T13 release uses the physical button chosen at press time', len(e) >= 2 and e[0][1] == '3' and e[-1][1] == '3', str(e))
    gs('false')
    time.sleep(0.8)
    n0 = len(evs(A))
    click_at(wa['id'], 'frame', 150, 150, 1)
    wait_events(A, n0 + 2)
    R.check('T13 right-handed: button 1 stays 1', evs(A)[n0][1] == '1', str(evs(A)[n0:]))

    # ---- T14 stall: SIGSTOP the private shell (recorded pid) and let not-after pass; nothing may inject on resume
    sp = int(open(os.path.join(W, 'shell.pid')).read())
    n0 = len(evs(A))
    results = []

    def caller():
        try:
            results.append(click_at(wa['id'], 'frame', 160, 160, 1, opts(not_after_ms=150), None))
        except HelperError as e:
            results.append(e)
    th = [threading.Thread(target=caller) for _ in range(4)]
    os.kill(sp, signal.SIGSTOP)
    try:
        for x in th:
            x.start()
        time.sleep(0.6)       # well past the 150 ms deadline
    finally:
        os.kill(sp, signal.SIGCONT)
    for x in th:
        x.join(5)
    time.sleep(0.5)
    outs = [r['outcome'] if isinstance(r, dict) else str(r) for r in results]
    R.check('T14 stalled shell: 4 queued clicks all expired, 0 injected after not-after',
            len(outs) == 4 and all(o == 'expired' for o in outs) and len(evs(A)) == n0, f'{outs} events+={len(evs(A)) - n0}')

    # ---- T14b Q-2: removing the pointer cap mid-drag releases the button within about a second
    import json
    cfg = os.path.join(W, 'config', 'gorca', 'shell-helper.json')
    exe = os.path.realpath(sys.executable)
    cg = open('/proc/self/cgroup').read().strip().split('::', 1)[1]

    def write_rules(rules):
        tmp = cfg + '.tmp'
        with open(tmp, 'w') as fh:
            json.dump({'allowed': rules + [{'exe': exe.replace('py-a', 'py-b'), 'cgroup': cg}]}, fh)
        os.chmod(tmp, 0o600)
        os.replace(tmp, cfg)
    wa = rewin('PA')
    move(wa['id'], 'frame', 220, 220)
    n0 = len(evs(A))
    button(1, 'press')
    time.sleep(1.2)           # the config stamp has microsecond resolution, but give the watch a tick
    write_rules([{'exe': exe, 'cgroup': cg, 'caps': ['capture']}])
    t0 = time.time()
    ok = wait_for(lambda: any(x[0] == 'release' for x in evs(A)[n0:]), 4)
    R.check('T14b removing the pointer cap mid-drag releases the held button within ~2 s', ok and time.time() - t0 < 2.5, f'{time.time() - t0:.1f} s {evs(A)[n0:]}')
    e, _ = err(get_pointer)
    R.check('T14b the caller now has no pointer cap', e and e.name == 'AccessDenied', str(e))
    R.check('T14b PointerRelease still works without the cap (releasing is always safe)', err(release)[0] is None)
    write_rules([{'exe': exe, 'cgroup': cg, 'caps': ['capture', 'pointer']}])
    time.sleep(1.2)

    # ---- T15 slow: 125 s backstop
    if SLOW:
        n0 = len(evs(A))
        button(1, 'press')
        t0 = time.time()
        ok = wait_for(lambda: any(x[0] == 'release' for x in evs(A)[n0:]), 135, 0.5)
        dt = time.time() - t0
        R.check('T15 backstop releases a held button at 125 s +-2 s', ok and 123 <= dt <= 128, f'{dt:.1f} s')

    # ---- T16 disable() releases holds (hot reload)
    n0 = len(evs(A))
    button(1, 'press')
    time.sleep(0.2)
    env = dict(os.environ, XDG_CONFIG_HOME=os.path.join(W, 'config'), GSETTINGS_BACKEND='keyfile')
    subprocess.run(['gsettings', 'set', 'org.gnome.shell', 'enabled-extensions', "['mouse-probe@gorca']"], env=env)
    ok = wait_for(lambda: any(x[0] == 'release' for x in evs(A)[n0:]), 5)
    R.check('T16 disabling the extension releases a held button', ok, str(evs(A)[n0:]))
    gone = wait_for(lambda: err(ping)[0] is not None, 5)
    R.check('T16 helper name disappears after disable', gone)
    subprocess.run(['gsettings', 'set', 'org.gnome.shell', 'enabled-extensions', "['gorca-helper@gorca','mouse-probe@gorca']"], env=env)
    back = wait_for(lambda: err(ping)[0] is None, 8)
    R.check('T16 helper comes back after enable (dev hot reload)', back)

    log = open(os.path.join(W, 'shell.log')).read()
    crit = [l for l in log.split('\n') if 'CRITICAL' in l or 'JS ERROR' in l]
    R.check('no CRITICAL / JS ERROR line in the shell log (F-1)', not crit, '; '.join(crit[:3]))
    for c in clients:
        c.kill()
    return R.summary()


if __name__ == '__main__':
    try:
        rc = main()
    finally:
        for c in clients:
            c.kill()
    sys.exit(rc)
