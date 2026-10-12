"""Browser drivers with TRUSTED input: Chromium via CDP Input.* (headless=new, no window), Firefox via Marionette actions (headed).
Both expose the same tiny API so scenarios are written once."""
import os, sys, json, time, subprocess, tempfile
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '..', 'probes'))

CHROME = os.environ.get('CHROME', os.path.expanduser('~/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome'))
KEYS = {  # name: (key, code, vk, wd)   wd = WebDriver private-use codepoint
    'Tab': ('Tab', 'Tab', 9, ''), 'Escape': ('Escape', 'Escape', 27, ''), 'Enter': ('Enter', 'Enter', 13, ''),
    'ArrowDown': ('ArrowDown', 'ArrowDown', 40, ''), 'ArrowUp': ('ArrowUp', 'ArrowUp', 38, ''),
    'ArrowLeft': ('ArrowLeft', 'ArrowLeft', 37, ''), 'ArrowRight': ('ArrowRight', 'ArrowRight', 39, ''),
    'Home': ('Home', 'Home', 36, ''), 'End': ('End', 'End', 35, ''),
    'PageDown': ('PageDown', 'PageDown', 34, ''), 'PageUp': ('PageUp', 'PageUp', 33, ''), 'Space': (' ', 'Space', 32, ' '),
}
RECT_JS = ("(sel)=>{const e=document.querySelector(sel); if(!e) return null; const v=innerHeight;"
           "let r=e.getBoundingClientRect(); if(r.bottom>v||r.top<0){e.scrollIntoView({block:'nearest'}); r=e.getBoundingClientRect();}"
           "return [r.x+r.width/2, r.y+r.height/2];}")


class Base:
    def center(self, sel):
        c = self.eval(f"({RECT_JS})({json.dumps(sel)})")
        if not c:
            raise RuntimeError(f'selector not found: {sel}')
        return c


class Chrome(Base):
    name = 'chrome'
    def __init__(self):
        from cdp_probe import CDP
        self.c = CDP(CHROME, extra=('--headless=new', '--force-renderer-accessibility', '--window-size=1000,800'))
        m, _ = self.c.call('Target.createTarget', {'url': 'about:blank'})
        m, _ = self.c.call('Target.attachToTarget', {'targetId': m['result']['targetId'], 'flatten': True})
        self.s = m['result']['sessionId']
        self.c.call('Page.enable', session=self.s)
        self.c.call('Emulation.setFocusEmulationEnabled', {'enabled': True}, session=self.s)
    def goto(self, url):
        self.c.events.clear()
        self.c.call('Page.navigate', {'url': url}, session=self.s)
        t0 = time.time()
        while time.time() - t0 < 20 and not any(e[1].get('method') == 'Page.loadEventFired' for e in list(self.c.events)):
            time.sleep(0.02)
    def eval(self, js):
        r, _ = self.c.call('Runtime.evaluate', {'expression': js, 'returnByValue': True, 'awaitPromise': False}, session=self.s)
        res = r.get('result', {})
        if 'exceptionDetails' in res:
            raise RuntimeError(json.dumps(res['exceptionDetails'])[:300])
        return res.get('result', {}).get('value')
    def _mouse(self, t, x, y, **kw):
        self.c.call('Input.dispatchMouseEvent', {'type': t, 'x': x, 'y': y, **kw}, session=self.s)
    def click(self, sel):
        x, y = self.center(sel)
        self._mouse('mouseMoved', x, y)
        self._mouse('mousePressed', x, y, button='left', clickCount=1, buttons=1)
        self._mouse('mouseReleased', x, y, button='left', clickCount=1)
    def key(self, name):
        k, code, vk, _ = KEYS[name]
        extra = {'text': '\r'} if name == 'Enter' else {}
        for t in (('keyDown' if extra else 'rawKeyDown'), 'keyUp'):   # Enter needs a 'char' (keyDown+text) for implicit form submission
            self.c.call('Input.dispatchKeyEvent', {'type': t, 'key': k, 'code': code, 'windowsVirtualKeyCode': vk, 'nativeVirtualKeyCode': vk, **(extra if t != 'keyUp' else {})}, session=self.s)
    def type(self, text):
        for ch in text:
            vk = ord(ch.upper())
            self.c.call('Input.dispatchKeyEvent', {'type': 'keyDown', 'key': ch, 'text': ch, 'windowsVirtualKeyCode': vk}, session=self.s)
            self.c.call('Input.dispatchKeyEvent', {'type': 'keyUp', 'key': ch, 'windowsVirtualKeyCode': vk}, session=self.s)
    def wheel(self, sel, dy):
        x, y = self.center(sel)
        self._mouse('mouseWheel', x, y, deltaX=0, deltaY=dy)
    def close(self):
        self.c.close()


class Firefox(Base):
    name = 'firefox'
    synthetic_scroll = False
    def __init__(self):
        from marionette_client import Marionette
        # Runs inside a PRIVATE headless gnome-shell (run_characterize.sh): the window is always visible, no focus is stolen.
        # (On the user's compositor an occluded window goes visibilityState=hidden and Gecko throttles a11y updates: see report.)
        self.m = Marionette(port=28291, headless=False, extra_prefs='user_pref("browser.aboutwelcome.enabled", false);\nuser_pref("browser.startup.homepage_override.mstone","ignore");\n')
        self.m.call('WebDriver:NewSession', {'capabilities': {'alwaysMatch': {}}})
        self.m.call('WebDriver:SetWindowRect', {'width': 1000, 'height': 800})
    def goto(self, url):
        self.m.call('WebDriver:Navigate', {'url': url})
    def eval(self, js):
        r = self.m.script(f'return ({js})')
        return r.get('value') if isinstance(r, dict) else r
    def _act(self, actions):
        self.m.call('WebDriver:PerformActions', {'actions': actions})
    def click(self, sel):
        x, y = self.center(sel)
        self._act([{'type': 'pointer', 'id': 'mouse', 'parameters': {'pointerType': 'mouse'}, 'actions': [
            {'type': 'pointerMove', 'x': int(x), 'y': int(y), 'origin': 'viewport'}, {'type': 'pointerDown', 'button': 0}, {'type': 'pointerUp', 'button': 0}]}])
    def key(self, name):
        wd = KEYS[name][3]
        self._act([{'type': 'key', 'id': 'kb', 'actions': [{'type': 'keyDown', 'value': wd}, {'type': 'keyUp', 'value': wd}]}])
    def type(self, text):
        self._act([{'type': 'key', 'id': 'kb', 'actions': [a for ch in text for a in ({'type': 'keyDown', 'value': ch}, {'type': 'keyUp', 'value': ch})]}])
    def wheel(self, sel, dy):
        x, y = self.center(sel)
        before = self.eval(f"document.querySelector({json.dumps(sel)}).scrollTop")
        self._wheel(x, y, dy)
        time.sleep(0.3)
        if self.eval(f"document.querySelector({json.dumps(sel)}).scrollTop") == before:   # headed wheel actions can be dropped
            self.synthetic_scroll = True
            self.eval(f"document.querySelector({json.dumps(sel)}).scrollBy(0,{dy})")
    def _wheel(self, x, y, dy):
        self._act([{'type': 'wheel', 'id': 'w', 'actions': [{'type': 'scroll', 'x': int(x), 'y': int(y), 'deltaX': 0, 'deltaY': int(dy), 'origin': 'viewport'}]}])
    def close(self):
        self.m.close()
