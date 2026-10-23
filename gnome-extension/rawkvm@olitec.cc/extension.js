// RawKVM edge bridge for GNOME Shell 46 (ESM).
//
// Under Wayland only the compositor knows and moves the pointer. This
// extension exports a minimal D-Bus object on the session bus so the RawKVM
// agent (Rust, crates/agent-linux) can run the edge logic itself:
//   Monitors()      -> a(iiiidb)  x, y, w, h, scale, primary (logical px)
//   Watch(b)                      start/stop sampling the pointer; a client
//                                 must renew Watch(true) at least every 3 s
//   Pointer(ii)     signal        pointer position, every ~8 ms while
//                                 watched, only when it moved
//   GetPointer()    -> (ii)       one sample (diagnostics)
//   Warp(ii)                      move the pointer
//   MonitorsChanged signal
// See docs/gnome-plan.md.

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Clutter from 'gi://Clutter';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const BUS_NAME = 'cc.olitec.RawKvm.Shell';
const OBJECT_PATH = '/cc/olitec/RawKvm/Shell';
const INTERFACE = `
<node>
  <interface name="cc.olitec.RawKvm.Shell1">
    <method name="Monitors">
      <arg type="a(iiiidb)" direction="out" name="monitors"/>
    </method>
    <method name="Watch">
      <arg type="b" direction="in" name="on"/>
    </method>
    <method name="GetPointer">
      <arg type="i" direction="out" name="x"/>
      <arg type="i" direction="out" name="y"/>
    </method>
    <method name="Warp">
      <arg type="i" direction="in" name="x"/>
      <arg type="i" direction="in" name="y"/>
    </method>
    <property name="Version" type="u" access="read"/>
    <signal name="Pointer">
      <arg type="i" name="x"/>
      <arg type="i" name="y"/>
    </signal>
    <signal name="MonitorsChanged"/>
  </interface>
</node>`;

const SAMPLE_MS = 8;
const WATCH_LEASE_US = 3 * 1000 * 1000;

class Bridge {
    constructor() {
        this._dbus = Gio.DBusExportedObject.wrapJSObject(INTERFACE, this);
        this._timer = 0;
        this._last = null;
        this._leaseUntil = 0;
    }

    get Version() {
        return 1;
    }

    Monitors() {
        const display = global.display;
        const primary = display.get_primary_monitor();
        const out = [];
        for (let i = 0; i < display.get_n_monitors(); i++) {
            const g = display.get_monitor_geometry(i);
            out.push([g.x, g.y, g.width, g.height, display.get_monitor_scale(i), i === primary]);
        }
        return out;
    }

    Watch(on) {
        if (!on) {
            this._stop();
            return;
        }
        this._leaseUntil = GLib.get_monotonic_time() + WATCH_LEASE_US;
        if (this._timer)
            return;
        this._last = null;
        this._timer = GLib.timeout_add(GLib.PRIORITY_HIGH, SAMPLE_MS, () => this._sample());
    }

    GetPointer() {
        const [x, y] = global.get_pointer();
        return [x, y];
    }

    Warp(x, y) {
        const seat = Clutter.get_default_backend().get_default_seat();
        seat.warp_pointer(x, y);
        this._last = null; // report the new position on the next sample
    }

    _sample() {
        if (GLib.get_monotonic_time() > this._leaseUntil) {
            // The client went away without Watch(false).
            this._timer = 0;
            return GLib.SOURCE_REMOVE;
        }
        const [x, y] = global.get_pointer();
        if (!this._last || this._last[0] !== x || this._last[1] !== y) {
            this._last = [x, y];
            this._dbus.emit_signal('Pointer', new GLib.Variant('(ii)', [x, y]));
        }
        return GLib.SOURCE_CONTINUE;
    }

    _stop() {
        if (this._timer) {
            GLib.source_remove(this._timer);
            this._timer = 0;
        }
        this._leaseUntil = 0;
    }

    monitorsChanged() {
        this._dbus.emit_signal('MonitorsChanged', null);
    }

    export(connection) {
        this._dbus.export(connection, OBJECT_PATH);
        this._exported = true;
    }

    destroy() {
        this._stop();
        if (this._exported)
            this._dbus.unexport();
        this._exported = false;
    }
}

export default class RawKvmExtension extends Extension {
    enable() {
        this._bridge = new Bridge();
        this._monitorsId = Main.layoutManager.connect('monitors-changed',
            () => this._bridge?.monitorsChanged());
        this._ownerId = Gio.bus_own_name(
            Gio.BusType.SESSION,
            BUS_NAME,
            Gio.BusNameOwnerFlags.NONE,
            connection => this._bridge?.export(connection),
            null,
            null);
    }

    disable() {
        if (this._ownerId) {
            Gio.bus_unown_name(this._ownerId);
            this._ownerId = 0;
        }
        if (this._monitorsId) {
            Main.layoutManager.disconnect(this._monitorsId);
            this._monitorsId = 0;
        }
        this._bridge?.destroy();
        this._bridge = null;
    }
}
