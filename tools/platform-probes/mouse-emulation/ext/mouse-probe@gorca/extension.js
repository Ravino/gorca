// PROBE ONLY. Virtual pointer from inside gnome-shell, exported as org.gorca.MouseProbe.
import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const IFACE = `<node><interface name="org.gorca.MouseProbe">
<method name="MoveAbs"><arg type="d" direction="in" name="x"/><arg type="d" direction="in" name="y"/><arg type="x" direction="out" name="us"/></method>
<method name="MoveRel"><arg type="d" direction="in" name="dx"/><arg type="d" direction="in" name="dy"/></method>
<method name="Button"><arg type="u" direction="in" name="button"/><arg type="b" direction="in" name="pressed"/></method>
<method name="Click"><arg type="u" direction="in" name="button"/><arg type="x" direction="out" name="us"/></method>
<method name="HideOverview"/>
<method name="MoveFrame"><arg type="t" direction="in" name="id"/><arg type="i" direction="in" name="x"/><arg type="i" direction="in" name="y"/></method>
<method name="Pointer"><arg type="i" direction="out" name="x"/><arg type="i" direction="out" name="y"/></method>
<method name="Frames"><arg type="a(sstiiii)" direction="out" name="w"/></method>
<method name="Monitors"><arg type="a(iiiid)" direction="out" name="m"/></method>
</interface></node>`;

export default class MouseProbe extends Extension {
    enable() {
        const seat = global.stage.context.get_backend().get_default_seat();
        this._dev = seat.create_virtual_device(Clutter.InputDeviceType.POINTER_DEVICE);
        this._obj = Gio.DBusExportedObject.wrapJSObject(IFACE, this);
        this._obj.export(Gio.DBus.session, '/org/gorca/MouseProbe');
        this._own = Gio.bus_own_name_on_connection(Gio.DBus.session, 'org.gorca.MouseProbe',
            Gio.BusNameOwnerFlags.NONE, null, null);
        console.log('mouse-probe: ready');
    }
    disable() {
        Gio.bus_unown_name(this._own);
        this._obj.unexport();
        this._dev = null;
    }
    _now() { return GLib.get_monotonic_time(); }
    MoveAbs(x, y) {
        const t0 = this._now();
        this._dev.notify_absolute_motion(Clutter.CURRENT_TIME, x, y);
        return this._now() - t0;
    }
    MoveRel(dx, dy) { this._dev.notify_relative_motion(Clutter.CURRENT_TIME, dx, dy); }
    Button(b, pressed) {
        this._dev.notify_button(Clutter.CURRENT_TIME, b,
            pressed ? Clutter.ButtonState.PRESSED : Clutter.ButtonState.RELEASED);
    }
    Click(b) {
        const t0 = this._now();
        this.Button(b, true); this.Button(b, false);
        return this._now() - t0;
    }
    MoveFrame(id, x, y) {
        for (const a of global.get_window_actors())
            if (a.meta_window.get_id() === id) a.meta_window.move_frame(true, x, y);
    }
    HideOverview() { Main.overview.hide(); }
    Pointer() { const [x, y] = global.get_pointer(); return [x, y]; }
    Frames() {
        return global.get_window_actors().map(a => {
            const w = a.meta_window, f = w.get_frame_rect();
            return [w.get_title() ?? '', w.get_wm_class() ?? '', w.get_id(), f.x, f.y, f.width, f.height];
        });
    }
    Monitors() {
        const d = global.display, n = d.get_n_monitors(), out = [];
        for (let i = 0; i < n; i++) {
            const g = d.get_monitor_geometry(i);
            out.push([g.x, g.y, g.width, g.height, d.get_monitor_scale(i)]);
        }
        return out;
    }
}
