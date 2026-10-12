// gorca-helper@gorca: D-Bus service org.gorca.ShellHelper on the session bus.
// Runs inside gnome-shell; every handler must return quickly (main loop is shared
// with the compositor). No pixels are written to disk.
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Mtk from 'gi://Mtk';
import Shell from 'gi://Shell';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const VERSION = 1;
const BUS_NAME = 'org.gorca.ShellHelper';
const OBJECT_PATH = '/org/gorca/ShellHelper';
const MAX_PIXELS = 2048 * 2048;     // logical area cap per request
const RATE_PER_SEC = 100;           // token bucket, burst = 2x
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

export class ShellHelperService {
    constructor(extDir) {
        this._extDir = extDir;
        this._tokens = RATE_PER_SEC * 2;
        this._tokenTime = GLib.get_monotonic_time();
        this._config = null;
        this._configStamp = null;
        this._denied = 0;
        this._senders = new Map();
        this._trace = Gio.File.new_for_path(`${extDir}/.trace`).query_exists(null);
    }

    start() {
        this._dbus = Gio.DBusExportedObject.wrapJSObject(IFACE, this);
        this._dbus.export(Gio.DBus.session, OBJECT_PATH);
        this._noc = Gio.DBus.session.signal_subscribe(
            'org.freedesktop.DBus', 'org.freedesktop.DBus', 'NameOwnerChanged',
            '/org/freedesktop/DBus', null, Gio.DBusSignalFlags.NONE,
            (_c, _s, _o, _i, _n, params) => {
                const [name, , newOwner] = params.deepUnpack();
                if (name.startsWith(':') && newOwner === '')
                    this._dropSender(name);
            });
        this._ownerId = Gio.bus_own_name_on_connection(
            Gio.DBus.session, BUS_NAME, Gio.BusNameOwnerFlags.NONE,
            () => console.log(`gorca-helper: v${VERSION} owns ${BUS_NAME}`),
            () => console.warn(`gorca-helper: could not own ${BUS_NAME} (name squatted?)`));
    }

    stop() {
        if (this._ownerId) {
            Gio.bus_unown_name(this._ownerId);
            this._ownerId = 0;
        }
        if (this._noc) {
            Gio.DBus.session.signal_unsubscribe(this._noc);
            this._noc = 0;
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
                    .filter(r => typeof r.exe === 'string' && typeof r.cgroup === 'string');
                this._config = {allowed};
                this._configStamp = stamp;
            } catch (e) {
                console.warn(`gorca-helper: config ${path} unusable (${e.message}); denying everyone`);
                this._config = {allowed: []};
                this._configStamp = null;
            }
            return this._config;
        }
        return {allowed: []};
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

    _dropSender(name) {
        const e = this._senders.get(name);
        if (e) {
            try { GLib.close(e.fd); } catch (_e) { /* already closed */ }
            this._senders.delete(name);
        }
    }

    // Identity is bound to the bus connection through a pidfd obtained from the broker, so the pid
    // cannot be recycled while the entry lives. Returns normally if allowed, throws DeniedError otherwise.
    async _authorize(invocation) {
        const sender = invocation.get_sender();
        let id = this._senders.get(sender);
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
        for (const rule of this._loadConfig().allowed) {
            if (rule.exe === exe && rule.cgroup === cgroup &&
                (rule.label === undefined || rule.label === id.label))
                return {pid, exe};
        }
        this._denied++;
        console.warn(`gorca-helper: DENIED sender=${sender} pid=${pid} exe=${exe} cgroup=${cgroup} label=${id.label}`);
        throw new DeniedError(`caller ${exe} in ${cgroup} is not allow-listed`);
    }

    _rateLimit() {
        const now = GLib.get_monotonic_time();
        this._tokens = Math.min(RATE_PER_SEC * 2,
            this._tokens + (now - this._tokenTime) * RATE_PER_SEC / 1e6);
        this._tokenTime = now;
        if (this._tokens < 1)
            throw new Error('rate limit exceeded');
        this._tokens -= 1;
    }

    async _guard(invocation, fn) {
        try {
            await this._authorize(invocation);
            this._rateLimit();
            if (Main.sessionMode.isLocked)
                throw new Error('session is locked');
            invocation.return_value(await fn());
        } catch (e) {
            const name = e instanceof DeniedError
                ? 'org.gorca.ShellHelper.Error.AccessDenied'
                : 'org.gorca.ShellHelper.Error.Failed';
            invocation.return_dbus_error(name, String(e.message ?? e));
        }
    }

    // ---- methods --------------------------------------------------------

    async PingAsync(_params, invocation) {
        await this._guard(invocation, () => new GLib.Variant('(u)', [VERSION]));
    }

    async GetWindowsAsync(_params, invocation) {
        await this._guard(invocation, () => new GLib.Variant('(aa{sv})', [this._windows()]));
    }

    async GetMonitorsAsync(_params, invocation) {
        await this._guard(invocation, () => new GLib.Variant('(aa{sv})', [this._monitors()]));
    }

    async CaptureAreaAsync(params, invocation) {
        const [x, y, w, h, opts] = params;
        await this._guard(invocation, async () => {
            const [info, data] = await this._capture(x, y, w, h, opts);
            return new GLib.Variant('((uuusd)ay)', [info, data]);
        });
    }

    async CaptureWindowAreaAsync(params, invocation) {
        const [id, x, y, w, h, opts] = params;
        await this._guard(invocation, async () => {
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
        for (let i = 0; i < n; i++) {
            const g = global.display.get_monitor_geometry(i);
            out.push({
                'index': v.i(i),
                'x': v.i(g.x), 'y': v.i(g.y), 'w': v.i(g.width), 'h': v.i(g.height),
                'scale': v.d(global.display.get_monitor_scale(i)),
                'primary': v.b(i === global.display.get_primary_monitor()),
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
