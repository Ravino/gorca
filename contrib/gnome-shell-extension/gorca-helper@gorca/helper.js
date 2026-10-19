// gorca-helper@gorca: D-Bus service org.gorca.ShellHelper on the session bus.
// Runs inside gnome-shell; every handler must return quickly (main loop is shared
// with the compositor). No pixels are written to disk.
import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Mtk from 'gi://Mtk';
import Shell from 'gi://Shell';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const VERSION = 2;
const BUS_NAME = 'org.gorca.ShellHelper';
const OBJECT_PATH = '/org/gorca/ShellHelper';
const MAX_PIXELS = 2048 * 2048;     // logical area cap per request
const RATE_PER_SEC = 100;           // token bucket, burst = 2x
const PTR_RATE_PER_SEC = 20;        // H-1: pointer methods, burst 2x (a count=2 click costs 2)
const DENIED_RATE_PER_SEC = 10;     // H-1a: ONE global bucket for all callers that are not (yet) allow-listed, burst 2x
const DENIED_TTL_US = 5e6;          // negative authorization cache lifetime (re-reads /proc after this)
const HOLD_BACKSTOP_MS = 125000;    // H-3: gorca releases at 120 s, the helper at 125 s
const DOUBLE_CLICK_GAP_MS = 30;
const MAX_COORD = 65535;
const KNOWN_CAPS = ['capture', 'pointer'];
const SYSTEM_CONFIG_PATH = '/etc/gorca/shell-helper.json';
const CONFIG_PATH = GLib.build_filenamev([GLib.get_user_config_dir(), 'gorca', 'shell-helper.json']);

const MY_UID = Gio.File.new_for_path('/proc/self').query_info('unix::uid', 0, null)
    .get_attribute_uint32('unix::uid');

const IFACE = `
<node>
  <interface name="org.gorca.ShellHelper">
    <method name="Ping">
      <arg type="u" direction="out" name="version"/>
    </method>
    <method name="Capabilities">
      <arg type="as" direction="out" name="caps"/>
    </method>
    <method name="GetPointer">
      <arg type="a{sv}" direction="out" name="pointer"/>
    </method>
    <method name="PointerMove">
      <arg type="t" direction="in" name="window"/>
      <arg type="s" direction="in" name="origin"/>
      <arg type="d" direction="in" name="x"/>
      <arg type="d" direction="in" name="y"/>
      <arg type="a{sv}" direction="in" name="options"/>
      <arg type="a{sv}" direction="out" name="reply"/>
    </method>
    <method name="PointerClickAt">
      <arg type="t" direction="in" name="window"/>
      <arg type="s" direction="in" name="origin"/>
      <arg type="d" direction="in" name="x"/>
      <arg type="d" direction="in" name="y"/>
      <arg type="u" direction="in" name="button"/>
      <arg type="a{sv}" direction="in" name="options"/>
      <arg type="a{sv}" direction="out" name="reply"/>
    </method>
    <method name="PointerButton">
      <arg type="u" direction="in" name="button"/>
      <arg type="s" direction="in" name="action"/>
      <arg type="a{sv}" direction="in" name="options"/>
      <arg type="a{sv}" direction="out" name="reply"/>
    </method>
    <method name="PointerRelease">
      <arg type="u" direction="out" name="released"/>
    </method>
    <method name="GetWindows">
      <arg type="aa{sv}" direction="out" name="windows"/>
    </method>
    <method name="GetMonitors">
      <arg type="aa{sv}" direction="out" name="monitors"/>
    </method>
    <method name="CaptureArea">
      <arg type="i" direction="in" name="x"/>
      <arg type="i" direction="in" name="y"/>
      <arg type="i" direction="in" name="width"/>
      <arg type="i" direction="in" name="height"/>
      <arg type="a{sv}" direction="in" name="options"/>
      <arg type="(uuusd)" direction="out" name="info"/>
      <arg type="ay" direction="out" name="pixels"/>
    </method>
    <method name="CaptureWindowArea">
      <arg type="t" direction="in" name="window_id"/>
      <arg type="i" direction="in" name="x"/>
      <arg type="i" direction="in" name="y"/>
      <arg type="i" direction="in" name="width"/>
      <arg type="i" direction="in" name="height"/>
      <arg type="a{sv}" direction="in" name="options"/>
      <arg type="(uuusd)" direction="out" name="info"/>
      <arg type="ay" direction="out" name="pixels"/>
      <arg type="a{sv}" direction="out" name="meta"/>
    </method>
  </interface>
</node>`;

const v = {
    s: x => new GLib.Variant('s', x),
    u: x => new GLib.Variant('u', x),
    t: x => new GLib.Variant('t', x),
    i: x => new GLib.Variant('i', x),
    d: x => new GLib.Variant('d', x),
    b: x => new GLib.Variant('b', x),
};

class DeniedError extends Error {}
// Error.Failed with a short machine prefix: locked: rate: bad-args: no-window: busy:
class HelperError extends Error {
    constructor(prefix, msg) {
        super(`${prefix}: ${msg}`);
    }
}

const POPUP_TYPES = new Set([
    Meta.WindowType.DROPDOWN_MENU, Meta.WindowType.POPUP_MENU, Meta.WindowType.COMBO,
    Meta.WindowType.TOOLTIP, Meta.WindowType.MENU,
]);
const BUTTON_BIT = {1: 1, 2: 2, 3: 4};      // `held` bitmask: left 1, middle 2, right 4 (logical buttons)

const rectHas = (r, x, y) => x >= r.x && y >= r.y && x < r.x + r.width && y < r.y + r.height;

export class ShellHelperService {
    constructor(extDir) {
        this._extDir = extDir;
        this._tokens = RATE_PER_SEC * 2;
        this._tokenTime = GLib.get_monotonic_time();
        this._config = null;
        this._configStamp = null;
        this._denied = 0;
        this._senders = new Map();
        this._configGen = 0;
        this._denyTokens = DENIED_RATE_PER_SEC * 2;   // H-1a: global bucket, shared by every non-allowed sender
        this._denyTokenTime = GLib.get_monotonic_time();
        this._denyLogLast = 0;
        this._denyLogSuppressed = 0;
        this._gen = 0;                     // bumped by stop(): work started before it must not act after it
        this._stopped = true;
        this._lastPickUs = 0;
        this._ptrTokens = PTR_RATE_PER_SEC * 2;
        this._ptrTokenTime = GLib.get_monotonic_time();
        this._device = null;               // H-6: one lazily created POINTER_DEVICE
        this._holds = new Map();           // logical button -> {phys, source}
        this._holder = null;               // unique bus name that holds buttons (H-3)
        this._timers = new Set();          // H-7 double-click timers {id, sender, resolve}
        this._auditLast = 0;
        this._auditSuppressed = 0;
        this._layoutMode = 1;              // 1 logical, 2 physical (org.gnome.Mutter.DisplayConfig)
        this._mouseSettings = null;
        this._waylandSettings = undefined;
        this._trace = Gio.File.new_for_path(`${extDir}/.trace`).query_exists(null);
    }

    start() {
        this._stopped = false;
        this._dbus = Gio.DBusExportedObject.wrapJSObject(IFACE, this);
        this._dbus.export(Gio.DBus.session, OBJECT_PATH);
        this._noc = Gio.DBus.session.signal_subscribe(
            'org.freedesktop.DBus', 'org.freedesktop.DBus', 'NameOwnerChanged',
            '/org/freedesktop/DBus', null, Gio.DBusSignalFlags.NONE,
            (_c, _s, _o, _i, _n, params) => {
                const [name, , newOwner] = params.deepUnpack();
                if (name.startsWith(':') && newOwner === '')
                    this._senderGone(name);
            });
        this._sessionModeId = Main.sessionMode.connect('updated', () => {
            if (Main.sessionMode.isLocked)
                this._releaseEverything('session locked');
        });
        this._monitorsId = Main.layoutManager.connect('monitors-changed', () => this._fetchLayoutMode());
        this._fetchLayoutMode();
        this._ownerId = Gio.bus_own_name_on_connection(
            Gio.DBus.session, BUS_NAME, Gio.BusNameOwnerFlags.NONE,
            () => console.log(`gorca-helper: v${VERSION} owns ${BUS_NAME}`),
            () => console.warn(`gorca-helper: could not own ${BUS_NAME} (name squatted?)`));
    }

    stop() {
        this._stopped = true;
        this._gen++;
        if (this._ownerId) {
            Gio.bus_unown_name(this._ownerId);
            this._ownerId = 0;
        }
        if (this._noc) {
            Gio.DBus.session.signal_unsubscribe(this._noc);
            this._noc = 0;
        }
        if (this._sessionModeId) {
            Main.sessionMode.disconnect(this._sessionModeId);
            this._sessionModeId = 0;
        }
        if (this._monitorsId) {
            Main.layoutManager.disconnect(this._monitorsId);
            this._monitorsId = 0;
        }
        this._releaseEverything('disabled');
        if (this._device) {
            // H-6: destroy explicitly (Mutter releases anything still pressed when the device goes away)
            try {
                this._device.run_dispose();
            } catch (e) {
                console.warn(`gorca-helper: device dispose failed: ${e.message}`);
            }
            this._device = null;
        }
        for (const k of [...this._senders.keys()])
            this._dropSender(k);
        this._dbus?.unexport();
        this._dbus = null;
    }

    // ---- access control -------------------------------------------------

    _loadConfig() {
        // A root-owned /etc config, if present, is authoritative; otherwise the user config.
        for (const [path, owner] of [[SYSTEM_CONFIG_PATH, 0], [CONFIG_PATH, MY_UID]]) {
            const f = Gio.File.new_for_path(path);
            let info;
            try {
                info = f.query_info('time::modified,time::modified-usec,unix::uid,unix::mode', 0, null);
            } catch (e) {
                continue;
            }
            try {
                const stamp = `${path}:${info.get_attribute_uint64('time::modified')}.${info.get_attribute_uint32('time::modified-usec')}`;
                if (this._config && this._configStamp === stamp)
                    return this._config;
                if (info.get_attribute_uint32('unix::uid') !== owner ||
                    (info.get_attribute_uint32('unix::mode') & 0o022) !== 0)
                    throw new Error('unsafe owner/mode');
                const [, bytes] = f.load_contents(null);
                const cfg = JSON.parse(new TextDecoder().decode(bytes));
                // A rule needs BOTH the executable and the full cgroup path.
                const allowed = (Array.isArray(cfg.allowed) ? cfg.allowed : [])
                    .filter(r => typeof r.exe === 'string' && typeof r.cgroup === 'string')
                    .map(r => ({...r, caps: this._parseCaps(r)}));
                this._config = {allowed};
                this._configStamp = stamp;
                this._configGen++;
            } catch (e) {
                console.warn(`gorca-helper: config ${path} unusable (${e.message}); denying everyone`);
                this._config = {allowed: []};
                this._configStamp = null;
                this._configGen++;
            }
            return this._config;
        }
        return {allowed: []};
    }

    // AL-1: `caps` absent => ["capture"]; unknown strings ignored with one warning (per config load).
    _parseCaps(rule) {
        if (rule.caps === undefined)
            return new Set(['capture']);
        const caps = new Set();
        for (const c of Array.isArray(rule.caps) ? rule.caps : []) {
            if (KNOWN_CAPS.includes(c))
                caps.add(c);
            else
                console.warn(`gorca-helper: ignoring unknown cap ${JSON.stringify(c)} for ${rule.exe}`);
        }
        return caps;
    }

    _pidfdPid(fd) {
        // /proc/self/fdinfo/<pidfd> has "Pid:\t<pid>"; -1 once the process is gone.
        const [, b] = GLib.file_get_contents(`/proc/self/fdinfo/${fd}`);
        const m = /^Pid:\s*(-?\d+)/m.exec(new TextDecoder().decode(b));
        return m ? Number(m[1]) : -1;
    }

    _startTime(pid) {
        const [, b] = GLib.file_get_contents(`/proc/${pid}/stat`);
        const t = new TextDecoder().decode(b);
        // comm may contain spaces/parens: parse after the last ')'. Field 22 = index 19 after it.
        return t.slice(t.lastIndexOf(')') + 2).split(' ')[19];
    }

    // The caller's bus connection is gone: release what it held, drop its timers and caches.
    _senderGone(name) {
        if (this._holder === name)
            this._releaseHolds(`holder ${name} vanished`);
        this._cancelTimers(t => t.sender === name);
        this._dropSender(name);
    }

    // Identity cache only (also used when an entry is invalidated); never touches holds.
    _dropSender(name) {
        const e = this._senders.get(name);
        if (e) {
            try { GLib.close(e.fd); } catch (_e) { /* already closed */ }
            this._senders.delete(name);
        }
    }

    // Identity is bound to the bus connection through a pidfd obtained from the broker, so the pid
    // cannot be recycled while the entry lives. Returns normally if allowed, throws DeniedError otherwise.
    async _authorize(invocation, cap) {
        const gen = this._gen;
        const sender = invocation.get_sender();
        let id = this._senders.get(sender);
        // H-1a: a caller that is not (yet) known to be allow-listed is rate limited per sender BEFORE any
        // /proc, config or broker work; a cached negative decision answers without touching anything.
        if (!id?.allowed) {
            this._denyBucket();
            this._loadConfig();     // a changed config stamp bumps _configGen and so voids cached denials
            if (id?.denied && id.denied.gen === this._configGen && GLib.get_monotonic_time() < id.denied.until)
                throw new DeniedError(id.denied.msg);
        }
        if (!id) {
            const [reply, fds] = await new Promise((resolve, reject) => {
                Gio.DBus.session.call_with_unix_fd_list(
                    'org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus',
                    'GetConnectionCredentials', new GLib.Variant('(s)', [sender]),
                    new GLib.VariantType('(a{sv})'), Gio.DBusCallFlags.NONE, 500, null, null,
                    (c, res) => {
                        try {
                            resolve(c.call_with_unix_fd_list_finish(res));
                        } catch (e) {
                            reject(e);
                        }
                    });
            });
            if (gen !== this._gen)
                throw new HelperError('disabled', 'helper was disabled during authorization');
            const creds = reply.get_child_value(0);
            const lookup = k => creds.lookup_value(k, null);
            const handle = lookup('ProcessFD');
            if (!handle || !fds)
                throw new DeniedError('broker gave no pidfd (ProcessFD); refusing to fall back to pid');
            const fd = fds.get(handle.get_handle());
            const pid = this._pidfdPid(fd);
            if (pid <= 0) {
                GLib.close(fd);
                throw new DeniedError('caller already exited');
            }
            const label = lookup('LinuxSecurityLabel');
            id = {
                fd, pid,
                start: this._startTime(pid),
                label: label ? new TextDecoder().decode(label.get_data_as_bytes().toArray()) : '',
            };
            if (this._senders.size > 256)
                for (const k of [...this._senders.keys()])
                    this._dropSender(k);
            this._senders.set(sender, id);
        }
        const {pid} = id;
        let exe, cgroup;
        try {
            if (this._pidfdPid(id.fd) !== pid)
                throw new Error('process gone');
            if (this._startTime(pid) !== id.start)
                throw new Error('start time changed');
            exe = GLib.file_read_link(`/proc/${pid}/exe`).replace(/ \(deleted\)$/, '');
            const [, b] = GLib.file_get_contents(`/proc/${pid}/cgroup`);
            const line = new TextDecoder().decode(b).trim().split('\n').find(l => l.startsWith('0::'));
            cgroup = line ? line.slice(3) : '';
            // Re-check after reading: the pidfd must still refer to the same live process.
            if (this._pidfdPid(id.fd) !== pid)
                throw new Error('process gone');
        } catch (e) {
            this._dropSender(sender);
            throw new DeniedError(`identity check failed: ${e.message}`);
        }
        // AL-1: the FIRST matching rule decides; later matching rules are never merged.
        for (const rule of this._loadConfig().allowed) {
            if (rule.exe === exe && rule.cgroup === cgroup &&
                (rule.label === undefined || rule.label === id.label)) {
                id.allowed = true;
                id.denied = null;
                if (cap && !rule.caps.has(cap)) {
                    // Identity is fine, only this method's cap is missing: not cached as a sender denial.
                    this._denied++;
                    const key = `${this._configGen}:${cap}`;
                    if (id.loggedCap !== key) {
                        id.loggedCap = key;
                        this._logDenied(`DENIED sender=${sender} pid=${pid} exe=${exe} cap=${cap} missing`);
                    }
                    throw new DeniedError(`caller ${exe} lacks cap ${cap}`);
                }
                return {sender, pid, exe, caps: rule.caps};
            }
        }
        this._deny(sender, id, `caller ${exe} in ${cgroup} is not allow-listed`,
            `DENIED sender=${sender} pid=${pid} exe=${exe} cgroup=${cgroup} label=${id.label}`);
    }

    // Records a negative decision (cached until the config stamp changes, the sender vanishes or
    // DENIED_TTL_US passes), logs it at most once per (sender, config stamp), and throws.
    _deny(sender, id, msg, logLine) {
        id.allowed = false;
        const gen = this._configGen;
        id.denied = {gen, until: GLib.get_monotonic_time() + DENIED_TTL_US, msg};
        this._denied++;
        if (id.loggedGen !== gen) {
            id.loggedGen = gen;
            this._logDenied(logLine);
        }
        throw new DeniedError(msg);
    }

    _denyBucket() {
        const now = GLib.get_monotonic_time();
        this._denyTokens = Math.min(DENIED_RATE_PER_SEC * 2,
            this._denyTokens + (now - this._denyTokenTime) * DENIED_RATE_PER_SEC / 1e6);
        this._denyTokenTime = now;
        if (this._denyTokens < 1)
            throw new HelperError('rate', 'too many calls from callers that are not allow-listed');
        this._denyTokens -= 1;
    }

    // Log limit for denials is global too (many connections cannot multiply it): 1 line/s plus a counter.
    _logDenied(line) {
        const now = GLib.get_monotonic_time();
        if (now - this._denyLogLast < 1e6) {
            this._denyLogSuppressed++;
            return;
        }
        const sup = this._denyLogSuppressed ? ` (+${this._denyLogSuppressed} suppressed)` : '';
        this._denyLogLast = now;
        this._denyLogSuppressed = 0;
        console.warn(`gorca-helper: ${line}${sup}`);
    }

    _rateLimit() {
        const now = GLib.get_monotonic_time();
        this._tokens = Math.min(RATE_PER_SEC * 2,
            this._tokens + (now - this._tokenTime) * RATE_PER_SEC / 1e6);
        this._tokenTime = now;
        if (this._tokens < 1)
            throw new HelperError('rate', 'rate limit exceeded');
        this._tokens -= 1;
    }

    // H-1: separate bucket for pointer methods: 20/s, burst 40; a count = 2 click costs 2.
    _pointerRate(cost) {
        const now = GLib.get_monotonic_time();
        this._ptrTokens = Math.min(PTR_RATE_PER_SEC * 2,
            this._ptrTokens + (now - this._ptrTokenTime) * PTR_RATE_PER_SEC / 1e6);
        this._ptrTokenTime = now;
        if (this._ptrTokens < cost)
            throw new HelperError('rate', 'pointer rate limit exceeded');
        this._ptrTokens -= cost;
    }

    // cap: required capability or null (any allow-listed caller). opts.ptr: pointer bucket instead of the v1 one
    // (the handler takes it, because the cost depends on the arguments). fn(ctx) returns the reply Variant
    // (or a Promise of it).
    async _guard(invocation, cap, fn, opts = {}) {
        const gen = this._gen;
        try {
            const ctx = await this._authorize(invocation, cap);
            if (gen !== this._gen || this._stopped)
                throw new HelperError('disabled', 'helper is shutting down');
            if (!opts.ptr)
                this._rateLimit();
            if (Main.sessionMode.isLocked)
                throw new HelperError('locked', 'session is locked');
            const t0 = GLib.get_monotonic_time();
            const res = fn(ctx);       // handlers are synchronous up to their first await
            if (this._trace && cap === 'pointer') {
                const us = GLib.get_monotonic_time() - t0;
                console.log(`gorca-helper trace ${invocation.get_method_name()} handler_us=${us} pick_us=${this._lastPickUs}`);
            }
            invocation.return_value(await res);
        } catch (e) {
            const name = e instanceof DeniedError
                ? 'org.gorca.ShellHelper.Error.AccessDenied'
                : 'org.gorca.ShellHelper.Error.Failed';
            invocation.return_dbus_error(name, String(e.message ?? e));
        }
    }

    // ---- methods --------------------------------------------------------

    async PingAsync(_params, invocation) {
        await this._guard(invocation, null, () => new GLib.Variant('(u)', [VERSION]));
    }

    async CapabilitiesAsync(_params, invocation) {
        await this._guard(invocation, null, ctx => new GLib.Variant('(as)', [[...ctx.caps].sort()]));
    }

    async GetWindowsAsync(_params, invocation) {
        await this._guard(invocation, null, () => new GLib.Variant('(aa{sv})', [this._windows()]));
    }

    async GetMonitorsAsync(_params, invocation) {
        await this._guard(invocation, null, () => new GLib.Variant('(aa{sv})', [this._monitors()]));
    }

    async CaptureAreaAsync(params, invocation) {
        const [x, y, w, h, opts] = params;
        await this._guard(invocation, 'capture', async () => {
            const [info, data] = await this._capture(x, y, w, h, opts);
            return new GLib.Variant('((uuusd)ay)', [info, data]);
        });
    }

    async CaptureWindowAreaAsync(params, invocation) {
        const [id, x, y, w, h, opts] = params;
        await this._guard(invocation, 'capture', async () => {
            const win = this._findWindow(id);
            if (!win)
                throw new Error(`no such window ${id}`);
            const origin = opts.origin?.unpack?.() ?? 'frame';
            const base = origin === 'buffer' ? win.get_buffer_rect() : win.get_frame_rect();
            const [info, data] = await this._capture(base.x + x, base.y + y, w, h, opts);
            const meta = {
                'stage-x': v.i(base.x + x),
                'stage-y': v.i(base.y + y),
                'occluded': v.b(this._occluded(win, new Mtk.Rectangle({x: base.x + x, y: base.y + y, width: w, height: h}))),
            };
            return new GLib.Variant('((uuusd)aya{sv})', [info, data, meta]);
        });
    }

    // ---- pointer methods (cap "pointer") ----------------------------------------
    // Every handler is validation plus a few Clutter calls in one main-loop turn; the only deferred work is the
    // second click of a count = 2 click (a 30 ms timer, H-7).

    async GetPointerAsync(_params, invocation) {
        await this._guard(invocation, 'pointer', () => {
            this._pointerRate(1);
            const [x, y] = global.get_pointer();
            const picked = this._pickAt(x, y);
            const pw = picked === undefined ? this._topWindowAt(x, y) : this._ownerWindow(picked);
            const rect = new Mtk.Rectangle({x: Math.floor(x), y: Math.floor(y), width: 1, height: 1});
            const f = pw?.get_frame_rect();
            const out = {
                'x': v.d(x), 'y': v.d(y),
                'window': v.t(pw ? pw.get_id() : 0),
                'wx': v.d(f ? x - f.x : 0), 'wy': v.d(f ? y - f.y : 0),
                'monitor': v.i(global.display.get_monitor_index_for_rect(rect)),
            };
            return new GLib.Variant('(a{sv})', [out]);
        }, {ptr: true});
    }

    async PointerMoveAsync(params, invocation) {
        const [window, origin, x, y, opts] = params;
        await this._guard(invocation, 'pointer', ctx => {
            this._pointerRate(1);
            const notAfter = this._notAfter(opts);
            const r = this._target(window, origin, x, y, opts);
            if (this._expired(notAfter))
                return this._replyVariant(ctx, r, 'expired');
            if (r.outcome === 'done') {
                this._ensureDevice().notify_absolute_motion(Clutter.CURRENT_TIME, r.x, r.y);
                this._audit(ctx, 'move', 0, r);
            }
            return this._replyVariant(ctx, r, r.outcome);
        }, {ptr: true});
    }

    async PointerClickAtAsync(params, invocation) {
        const [window, origin, x, y, button, opts] = params;
        await this._guard(invocation, 'pointer', async ctx => {
            const count = this._count(opts);
            this._pointerRate(count);
            const btn = this._button(button);
            const notAfter = this._notAfter(opts);
            this._checkBusy(ctx);
            const r = this._target(window, origin, x, y, opts);
            if (this._expired(notAfter))
                return this._replyVariant(ctx, r, 'expired');
            if (r.outcome !== 'done')
                return this._replyVariant(ctx, r, r.outcome);
            this._ensureDevice().notify_absolute_motion(Clutter.CURRENT_TIME, r.x, r.y);
            this._click(ctx, btn, opts);
            this._audit(ctx, 'click', btn, r);
            let outcome = 'done';
            if (count === 2)
                outcome = await this._secondClick(ctx, btn, opts, notAfter, r);
            return this._replyVariant(ctx, r, outcome);
        }, {ptr: true});
    }

    async PointerButtonAsync(params, invocation) {
        const [button, action, opts] = params;
        await this._guard(invocation, 'pointer', async ctx => {
            if (!['click', 'press', 'release'].includes(action))
                throw new HelperError('bad-args', `unknown action ${JSON.stringify(action)}`);
            const count = action === 'click' ? this._count(opts) : 1;
            this._pointerRate(count);
            const btn = this._button(button);
            const notAfter = this._notAfter(opts);
            const [px, py] = global.get_pointer();
            const r = {x: px, y: py, windowAt: 0, occluded: false, clamped: false, outcome: 'done'};
            // A release never expires: a late release is always safe and a skipped one would leave a button down.
            if (action !== 'release' && this._expired(notAfter))
                return this._replyVariant(ctx, r, 'expired');
            if (action === 'release') {
                this._releaseOne(ctx.sender, btn);
            } else {
                this._checkBusy(ctx);
                if (action === 'press')
                    this._press(ctx, btn, opts);
                else
                    this._click(ctx, btn, opts);
            }
            this._audit(ctx, action, btn, r);
            let outcome = 'done';
            if (action === 'click' && count === 2)
                outcome = await this._secondClick(ctx, btn, opts, notAfter, r);
            return this._replyVariant(ctx, r, outcome);
        }, {ptr: true});
    }

    async PointerReleaseAsync(_params, invocation) {
        await this._guard(invocation, 'pointer', ctx => {
            this._pointerRate(1);
            const n = this._holder === ctx.sender ? this._releaseHolds('PointerRelease') : 0;
            return new GLib.Variant('(u)', [n]);
        }, {ptr: true});
    }

    // ---- pointer implementation ---------------------------------------------------

    _ensureDevice() {
        if (this._stopped)
            throw new HelperError('disabled', 'helper is shutting down');
        if (!this._device) {
            const seat = global.stage.context.get_backend().get_default_seat();
            this._device = seat.create_virtual_device(Clutter.InputDeviceType.POINTER_DEVICE);
        }
        return this._device;
    }

    _button(b) {
        if (!Number.isInteger(b) || b < 1 || b > 3)
            throw new HelperError('bad-args', `button ${b} is not 1, 2 or 3`);
        return b;
    }

    _count(opts) {
        const c = opts.count ? opts.count.unpack() : 1;
        if (c !== 1 && c !== 2)
            throw new HelperError('bad-args', `count ${c} is not 1 or 2`);
        return c;
    }

    // C-1: `not-after` (CLOCK_MONOTONIC microseconds, x) is REQUIRED on every injecting method.
    _notAfter(opts) {
        const n = opts['not-after'];
        if (!n || n.get_type_string() !== 'x')
            throw new HelperError('bad-args', 'not-after (x, CLOCK_MONOTONIC microseconds) is required');
        return Number(n.unpack());
    }

    _expired(notAfter, slackUs = 0) {
        return GLib.get_monotonic_time() > notAfter + slackUs;
    }

    // opts.logical (default true): the primary button of a left-handed user is the physical right one.
    _physical(opts, btn) {
        const logical = opts?.logical ? opts.logical.unpack() : true;
        if (!logical || (btn !== 1 && btn !== 3))
            return btn;
        this._mouseSettings ??= new Gio.Settings({schema_id: 'org.gnome.desktop.peripherals.mouse'});
        if (!this._mouseSettings.get_boolean('left-handed'))
            return btn;
        return btn === 1 ? 3 : 1;
    }

    // Resolves and validates a target. Throws bad-args:/no-window:; otherwise returns the reply fields with
    // outcome 'done' | 'refused-outside' | 'refused-occluded'. Pure reads: nothing is injected here.
    _target(window, origin, x, y, opts) {
        if (!Number.isFinite(x) || !Number.isFinite(y) || Math.abs(x) > MAX_COORD || Math.abs(y) > MAX_COORD)
            throw new HelperError('bad-args', 'coordinates must be finite and within +-65535');
        if (!['frame', 'buffer', 'stage'].includes(origin))
            throw new HelperError('bad-args', `unknown origin ${JSON.stringify(origin)}`);
        const ifOcc = opts['if-occluded'] ? opts['if-occluded'].unpack() : 'refuse';
        if (ifOcc !== 'refuse' && ifOcc !== 'proceed')
            throw new HelperError('bad-args', `unknown if-occluded ${JSON.stringify(ifOcc)}`);
        const winId = Number(window);
        const win = winId ? this._findWindow(winId) : null;
        if (winId && !win)
            throw new HelperError('no-window', `no such window ${winId}`);
        let sx = x, sy = y;
        if (origin !== 'stage') {
            if (!win)
                throw new HelperError('bad-args', `origin ${origin} needs a window`);
            const base = origin === 'buffer' ? win.get_buffer_rect() : win.get_frame_rect();
            sx = base.x + x;
            sy = base.y + y;
        }
        const r = {x: sx, y: sy, windowAt: 0, occluded: false, clamped: !this._onStage(sx, sy), outcome: 'done'};
        const t0 = GLib.get_monotonic_time();
        const picked = this._pickAt(sx, sy);
        const pw = picked === undefined ? this._topWindowAt(sx, sy) : this._ownerWindow(picked);
        this._lastPickUs = GLib.get_monotonic_time() - t0;
        r.windowAt = pw ? pw.get_id() : 0;
        if (win) {
            // MI-5 window guard + pick-based occlusion; both use the popup family (C-2).
            const family = this._family(win);
            let inside = false;
            for (const w of family) {
                if (rectHas(w.get_frame_rect(), sx, sy) || rectHas(w.get_buffer_rect(), sx, sy)) {
                    inside = true;
                    break;
                }
            }
            if (!inside) {
                r.outcome = 'refused-outside';
                return r;
            }
            r.occluded = !(pw && family.has(pw));
            if (r.occluded && ifOcc === 'refuse')
                r.outcome = 'refused-occluded';
        } else if (pw) {
            // C-4 stage guard: the St element the caller resolved is gone, an app window is under the point.
            r.outcome = 'refused-outside';
        }
        return r;
    }

    _onStage(x, y) {
        const n = global.display.get_n_monitors();
        for (let i = 0; i < n; i++) {
            if (rectHas(global.display.get_monitor_geometry(i), x, y))
                return true;
        }
        return false;
    }

    // Reactive actor under the point, or undefined if the pick API is unavailable.
    _pickAt(x, y) {
        const st = global.stage;
        if (typeof st.get_actor_at_pos !== 'function')
            return undefined;
        return st.get_actor_at_pos(Clutter.PickMode.REACTIVE, x, y);
    }

    // The Meta window whose MetaWindowActor contains `actor`, or null (Shell UI, Overview clones, ...).
    _ownerWindow(actor) {
        for (let a = actor; a; a = a.get_parent()) {
            if (a instanceof Meta.WindowActor)
                return a.meta_window ?? null;
        }
        return null;
    }

    // Fallback (no pick API): topmost visible window whose frame contains the point, by stacking order.
    _topWindowAt(x, y) {
        const wins = global.display.sort_windows_by_stacking(
            global.get_window_actors().map(a => a.meta_window).filter(Boolean));
        for (let i = wins.length - 1; i >= 0; i--) {
            const w = wins[i];
            if (!w.minimized && w.showing_on_its_workspace() && rectHas(w.get_frame_rect(), x, y))
                return w;
        }
        return null;
    }

    // Popup family (C-2): `win` plus every window whose transient-for chain reaches it through popup-type
    // windows only. A NORMAL/DIALOG/MODAL_DIALOG transient is not a member (it is an occluder).
    _family(win) {
        const rootId = win.get_id();
        const fam = new Set([win]);
        for (const actor of global.get_window_actors()) {
            const c = actor.meta_window;
            if (!c || fam.has(c) || c.minimized)
                continue;
            let cur = c;
            for (let d = 0; d < 8 && cur; d++) {
                if (cur.get_id() === rootId) {
                    fam.add(c);
                    break;
                }
                if (!POPUP_TYPES.has(cur.get_window_type()))
                    break;
                cur = cur.get_transient_for();
            }
        }
        return fam;
    }

    _heldMask(sender) {
        let m = 0;
        if (this._holder === sender)
            for (const b of this._holds.keys())
                m |= BUTTON_BIT[b];
        return m;
    }

    _replyVariant(ctx, r, outcome) {
        return new GLib.Variant('(a{sv})', [{
            'x': v.d(r.x), 'y': v.d(r.y),
            'window-at': v.t(r.windowAt), 'occluded': v.b(r.occluded), 'clamped': v.b(r.clamped),
            'outcome': v.s(outcome), 'held': v.u(this._heldMask(ctx.sender)),
        }]);
    }

    // H-3: one holder at a time. Anything that presses a button fails busy: while another caller holds one.
    _checkBusy(ctx) {
        if (this._holds.size > 0 && this._holder !== ctx.sender)
            throw new HelperError('busy', 'another caller holds a button');
    }

    _emitButton(phys, pressed) {
        this._ensureDevice().notify_button(Clutter.CURRENT_TIME, phys,
            pressed ? Clutter.ButtonState.PRESSED : Clutter.ButtonState.RELEASED);
    }

    _press(ctx, btn, opts) {
        if (this._holds.has(btn))
            return;     // already held by this holder (_checkBusy ran): no-op, the reply reports `held`
        const phys = this._physical(opts, btn);
        this._emitButton(phys, true);
        const source = GLib.timeout_add(GLib.PRIORITY_DEFAULT, HOLD_BACKSTOP_MS, () => {
            console.warn(`gorca-helper: backstop released button ${btn} after ${HOLD_BACKSTOP_MS} ms`);
            this._releaseOne(this._holder, btn);
            return GLib.SOURCE_REMOVE;
        });
        this._holds.set(btn, {phys, source});
        this._holder = ctx.sender;
    }

    // Releases `btn` if `sender` is the holder and holds it. Returns 1 if a release was injected.
    _releaseOne(sender, btn) {
        if (this._holder !== sender)
            return 0;
        const h = this._holds.get(btn);
        if (!h)
            return 0;
        GLib.source_remove(h.source);
        this._holds.delete(btn);
        try {
            this._emitButton(h.phys, false);
        } catch (e) {
            console.warn(`gorca-helper: release failed: ${e.message}`);
        }
        if (this._holds.size === 0)
            this._holder = null;
        return 1;
    }

    // Releases every held button (whoever holds it). Returns how many. A no-op without holds.
    _releaseHolds(why) {
        let n = 0;
        for (const [btn, h] of [...this._holds]) {
            GLib.source_remove(h.source);
            this._holds.delete(btn);
            try {
                this._emitButton(h.phys, false);
            } catch (e) {
                console.warn(`gorca-helper: release failed: ${e.message}`);
            }
            n++;
        }
        this._holder = null;
        if (n)
            console.log(`gorca-helper: released ${n} held button(s): ${why}`);
        return n;
    }

    _releaseEverything(why) {
        this._releaseHolds(why);
        this._cancelTimers(() => true);
    }

    // press + release of one logical button (a click of a button the caller holds just releases it).
    _click(ctx, btn, opts) {
        if (this._holds.has(btn)) {
            this._releaseOne(ctx.sender, btn);
            return;
        }
        const phys = this._physical(opts, btn);
        this._emitButton(phys, true);
        this._emitButton(phys, false);
    }

    // H-7: the second click of count = 2 comes from a 30 ms timer after re-checking not-after + 30 ms, the lock
    // state and that the caller is still on the bus; the reply is sent after it (or with outcome expired).
    async _secondClick(ctx, btn, opts, notAfter, r) {
        const ok = await this._later(DOUBLE_CLICK_GAP_MS, ctx.sender);
        if (!ok || this._expired(notAfter, DOUBLE_CLICK_GAP_MS * 1000) || Main.sessionMode.isLocked)
            return 'expired';
        this._click(ctx, btn, opts);
        this._audit(ctx, 'click2', btn, r);
        return 'done';
    }

    _later(ms, sender) {
        return new Promise(resolve => {
            const rec = {sender, resolve, id: 0};
            rec.id = GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {
                this._timers.delete(rec);
                resolve(true);
                return GLib.SOURCE_REMOVE;
            });
            this._timers.add(rec);
        });
    }

    _cancelTimers(pred) {
        for (const rec of [...this._timers]) {
            if (!pred(rec))
                continue;
            GLib.source_remove(rec.id);
            this._timers.delete(rec);
            rec.resolve(false);
        }
    }

    // H-5: one journal line per injection, rate limited to 1/s with a suppressed counter.
    _audit(ctx, op, btn, r) {
        const now = GLib.get_monotonic_time();
        if (now - this._auditLast < 1e6) {
            this._auditSuppressed++;
            return;
        }
        const sup = this._auditSuppressed ? ` (+${this._auditSuppressed} suppressed)` : '';
        this._auditLast = now;
        this._auditSuppressed = 0;
        console.log(`gorca-helper: pointer ${op} b${btn} stage=(${r.x.toFixed(1)},${r.y.toFixed(1)}) ` +
            `win=${r.windowAt} outcome=${r.outcome} caller=${ctx.exe}${sup}`);
    }

    // Reads Mutter's layout mode once (and on monitor changes); no D-Bus call happens in a handler.
    _fetchLayoutMode() {
        Gio.DBus.session.call(
            'org.gnome.Mutter.DisplayConfig', '/org/gnome/Mutter/DisplayConfig', 'org.gnome.Mutter.DisplayConfig',
            'GetCurrentState', null, null, Gio.DBusCallFlags.NONE, 1000, null,
            (c, res) => {
                try {
                    const props = c.call_finish(res).get_child_value(3);   // (u serial, monitors, logical monitors, properties)
                    const lm = props.lookup_value('layout-mode', null);
                    if (lm)
                        this._layoutMode = lm.get_uint32();
                } catch (e) {
                    console.warn(`gorca-helper: layout-mode unavailable (${e.message}); assuming logical`);
                }
            });
    }

    // C-5: XWayland's global effective scale, as meta_xwayland_get_effective_scale (meta-xwayland.c): 1 in
    // physical layout mode, else round(xwayland-scaling-factor) if set, else ceil(highest logical monitor scale).
    _xScale() {
        if (this._layoutMode === 2)
            return 1;
        if (this._waylandSettings === undefined) {
            // The key exists in mutter main only; Debian's 48.7 schema does not have it (then the factor is unset).
            const schema = Gio.SettingsSchemaSource.get_default().lookup('org.gnome.mutter.wayland', true);
            this._waylandSettings = schema?.has_key('xwayland-scaling-factor')
                ? new Gio.Settings({settings_schema: schema}) : null;
        }
        const f = this._waylandSettings?.get_double('xwayland-scaling-factor') ?? 0;
        if (Math.abs(f) > 1e-6)
            return Math.round(f);
        let hi = 1;
        for (let i = 0; i < global.display.get_n_monitors(); i++)
            hi = Math.max(hi, global.display.get_monitor_scale(i));
        return Math.ceil(hi);
    }

    // ---- implementation ---------------------------------------------------

    _windows() {
        const focus = global.display.focus_window;
        const out = [];
        for (const actor of global.get_window_actors()) {
            const w = actor.meta_window;
            if (!w || w.get_window_type() === Meta.WindowType.DESKTOP)
                continue;
            const f = w.get_frame_rect();
            const b = w.get_buffer_rect();
            out.push({
                'id': v.t(w.get_id()),
                'stable-seq': v.u(w.get_stable_sequence()),
                'pid': v.u(Math.max(0, w.get_pid())),
                'wm-class': v.s(w.get_wm_class() ?? ''),
                'app-id': v.s(w.get_gtk_application_id() ?? w.get_sandboxed_app_id() ?? ''),
                'title': v.s(w.get_title() ?? ''),
                'type': v.u(w.get_window_type()),
                'client-type': v.s(w.get_client_type() === Meta.WindowClientType.WAYLAND ? 'wayland' : 'x11'),
                'frame-x': v.i(f.x), 'frame-y': v.i(f.y), 'frame-w': v.i(f.width), 'frame-h': v.i(f.height),
                'buffer-x': v.i(b.x), 'buffer-y': v.i(b.y), 'buffer-w': v.i(b.width), 'buffer-h': v.i(b.height),
                'transient-for': v.t(w.get_transient_for()?.get_id() ?? 0),
                'monitor': v.i(w.get_monitor()),
                'focused': v.b(w === focus),
                'minimized': v.b(w.minimized),
                'visible': v.b(actor.visible && !w.minimized),
            });
        }
        return out;
    }

    _monitors() {
        const mm = global.display.get_monitor_manager?.() ?? null;
        const out = [];
        const n = global.display.get_n_monitors();
        const xScale = this._xScale();
        for (let i = 0; i < n; i++) {
            const g = global.display.get_monitor_geometry(i);
            out.push({
                'index': v.i(i),
                'x': v.i(g.x), 'y': v.i(g.y), 'w': v.i(g.width), 'h': v.i(g.height),
                'scale': v.d(global.display.get_monitor_scale(i)),
                'primary': v.b(i === global.display.get_primary_monitor()),
                'x-scale': v.d(xScale),
            });
        }
        void mm;
        return out;
    }

    _findWindow(id) {
        for (const actor of global.get_window_actors()) {
            const w = actor.meta_window;
            if (w && BigInt(w.get_id()) === BigInt(id))
                return w;
        }
        return null;
    }

    // True if a window stacked above `win` overlaps `rect`, so a stage crop would
    // contain foreign pixels.
    _occluded(win, rect) {
        const wins = global.display.sort_windows_by_stacking(
            global.get_window_actors().map(a => a.meta_window).filter(Boolean));
        const idx = wins.indexOf(win);
        for (let i = idx + 1; i < wins.length; i++) {
            const o = wins[i];
            if (o.minimized || !o.showing_on_its_workspace())
                continue;
            const [hit] = rect.intersect(o.get_frame_rect());
            if (hit)
                return true;
        }
        return false;
    }

    // Validates the request and returns {rect, scale, pw, ph}.
    _plan(x, y, w, h) {
        if (!Number.isInteger(x) || !Number.isInteger(y) || w <= 0 || h <= 0 || w * h > MAX_PIXELS)
            throw new Error('invalid or too large area');
        const stage = global.stage;
        if (x < 0 || y < 0 || x + w > stage.width || y + h > stage.height)
            throw new Error('area outside the stage');
        const rect = new Mtk.Rectangle({x, y, width: w, height: h});
        const [ok, pw, ph, scale] = stage.get_capture_final_size(rect);
        if (!ok)
            throw new Error('area not on any monitor');
        return {rect, scale, pw, ph};
    }

    // Returns PNG bytes of the stage area (cursor never included). Shell.Screenshot paints
    // synchronously (the only main-thread cost) and encodes on a worker thread.
    // Raw pixels are NOT possible from JS: GI copies (array) in-parameters, so
    // Clutter.Stage.paint_to_buffer / Cogl.Texture.get_data cannot fill a Uint8Array.
    async _capture(x, y, w, h, _opts) {
        const plan = this._plan(x, y, w, h);
        const stream = Gio.MemoryOutputStream.new_resizable();
        const shot = new Shell.Screenshot();
        const t0 = GLib.get_monotonic_time();
        const pending = shot.screenshot_area(x, y, w, h, stream);   // paints + reads back synchronously
        const t1 = GLib.get_monotonic_time();
        await pending;                                              // PNG encode on a worker thread
        const t2 = GLib.get_monotonic_time();
        if (this._trace)
            console.log(`gorca-helper trace ${w}x${h} sync_paint_us=${t1 - t0} encode_wait_us=${t2 - t1}`);
        stream.close(null);
        const bytes = stream.steal_as_bytes().toArray();
        return [[plan.pw, plan.ph, 0, 'PNG', plan.scale], bytes];
    }
}
