// legionctl OSD: draws the on-screen display that legionctl-operator requests over D-Bus (org.legionctl.Osd.Show).
// One OSD at a time; a new request while it shows animates from the current state instead of restarting.

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const BUS_NAME = 'org.legionctl.Osd';
const OBJECT_PATH = '/org/legionctl/Osd';
const IFACE = `
<node>
  <interface name="org.legionctl.Osd">
    <method name="Show">
      <arg type="s" name="kind" direction="in"/>
      <arg type="s" name="value" direction="in"/>
      <arg type="s" name="detail" direction="in"/>
      <arg type="d" name="level" direction="in"/>
    </method>
  </interface>
</node>`;

const IN_MS = 180;
const VISIBLE_MS = 1200;
const OUT_MS = 200;
const COUNT_MS = 250;

// firmware modes in Fn+Q order, with the label and icon the OSD shows
const MODES = [
    ['low-power', 'Quiet', 'power-profile-power-saver-symbolic'],
    ['balanced', 'Balanced', 'power-profile-balanced-symbolic'],
    ['performance', 'Performance', 'power-profile-performance-symbolic'],
    ['max-power', 'Max power', 'power-profile-performance-symbolic'],
    ['custom', 'Custom', 'preferences-system-symbolic'],
];

function animationsEnabled() {
    return St.Settings.get().enable_animations;
}

// Fn keys are on the built-in keyboard, so the OSD shows on the built-in panel; with the lid closed, on the primary
function builtInMonitor() {
    const manager = global.backend.get_monitor_manager();
    const panel = manager.get_monitors().find(m => m.is_builtin() && m.is_active());
    const index = panel ? manager.get_monitor_for_connector(panel.get_connector()) : -1;
    return Main.layoutManager.monitors[index] ?? Main.layoutManager.primaryMonitor;
}

class Osd {
    constructor() {
        this._box = new St.BoxLayout({
            style_class: 'osd-window legionctl-osd',
            orientation: Clutter.Orientation.VERTICAL,
            opacity: 0,
            visible: false,
        });
        this._title = new St.Label({style_class: 'legionctl-osd-title'});
        this._detail = new St.Label({style_class: 'legionctl-osd-detail'});

        // mode: a strip of mode icons; the selected one grows
        this._strip = new St.BoxLayout({style_class: 'legionctl-osd-strip', x_align: Clutter.ActorAlign.CENTER});
        this._modeIcons = MODES.map(([, , icon]) => {
            const i = new St.Icon({icon_name: icon, style_class: 'legionctl-osd-mode'});
            i.set_pivot_point(0.5, 0.5);
            this._strip.add_child(i);
            return i;
        });

        // refresh: a counting number and a sweep line
        this._big = new St.Label({style_class: 'legionctl-osd-big'});
        this._sweep = new St.Widget({style_class: 'legionctl-osd-sweep', x_expand: false});

        // Fn lock: one icon that springs when it changes
        this._lock = new St.Icon({style_class: 'legionctl-osd-lock', x_align: Clutter.ActorAlign.CENTER});
        this._lock.set_pivot_point(0.5, 0.5);

        for (const child of [this._strip, this._big, this._sweep, this._lock, this._title, this._detail])
            this._box.add_child(child);
        Main.layoutManager.addTopChrome(this._box);

        this._hideId = 0;
        this._countId = 0;
        this._refresh = null;
    }

    destroy() {
        if (this._hideId)
            GLib.source_remove(this._hideId);
        if (this._countId)
            GLib.source_remove(this._countId);
        this._box.destroy();
    }

    _only(...shown) {
        for (const child of [this._strip, this._big, this._sweep, this._lock])
            child.visible = shown.includes(child);
    }

    _place() {
        const monitor = builtInMonitor();
        const [, natWidth] = this._box.get_preferred_width(-1);
        const [, natHeight] = this._box.get_preferred_height(natWidth);
        this._box.set_position(
            monitor.x + Math.floor((monitor.width - natWidth) / 2),
            monitor.y + Math.floor(monitor.height * 0.78 - natHeight / 2));
    }

    _present() {
        this._title.visible = this._title.text !== '';
        this._place();
        // also stops a fade-out that is in progress
        this._box.visible = true;
        this._box.ease({opacity: 255, duration: IN_MS, mode: Clutter.AnimationMode.EASE_OUT_QUAD});
        if (this._hideId)
            GLib.source_remove(this._hideId);
        this._hideId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, VISIBLE_MS, () => {
            this._hideId = 0;
            this._box.ease({
                opacity: 0,
                duration: OUT_MS,
                mode: Clutter.AnimationMode.EASE_IN_QUAD,
                onComplete: () => {
                    this._box.visible = false;
                },
            });
            return GLib.SOURCE_REMOVE;
        });
    }

    show(kind, value, detail) {
        if (kind === 'mode')
            this._showMode(value, detail);
        else if (kind === 'refresh')
            this._showRefresh(Number(value), detail);
        else if (kind === 'fnlock')
            this._showFnLock(value === 'on', detail);
        else
            return;
        this._present();
    }

    _showMode(mode, detail) {
        this._only(this._strip);
        // an unknown mode shows its raw name and selects no icon
        const index = MODES.findIndex(([id]) => id === mode);
        const label = index >= 0 ? MODES[index][1] : mode;
        this._title.text = `Thermal mode: ${label}`;
        this._detail.text = detail;
        this._modeIcons.forEach((icon, i) => {
            const selected = i === index;
            if (selected)
                icon.add_style_class_name('selected');
            else
                icon.remove_style_class_name('selected');
            icon.ease({
                scale_x: selected ? 1.25 : 1,
                scale_y: selected ? 1.25 : 1,
                duration: 220,
                mode: Clutter.AnimationMode.EASE_OUT_BACK,
            });
        });
    }

    _showRefresh(hz, detail) {
        this._only(this._big, this._sweep);
        this._title.text = '';
        this._detail.text = detail;
        const from = this._refresh ?? hz;
        this._refresh = hz;
        if (this._countId)
            GLib.source_remove(this._countId);
        if (!animationsEnabled() || from === hz) {
            this._big.text = `${hz} Hz`;
        } else {
            const start = GLib.get_monotonic_time();
            this._countId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 16, () => {
                const t = Math.min(1, (GLib.get_monotonic_time() - start) / 1000 / COUNT_MS);
                const eased = 1 - (1 - t) * (1 - t);
                this._big.text = `${Math.round(from + (hz - from) * eased)} Hz`;
                if (t < 1)
                    return GLib.SOURCE_CONTINUE;
                this._countId = 0;
                return GLib.SOURCE_REMOVE;
            });
        }
        // the sweep crosses faster at a higher rate
        this._sweep.remove_all_transitions();
        this._sweep.width = 0;
        this._sweep.ease({
            width: 220,
            duration: Math.max(120, Math.round(36000 / Math.max(hz, 1))),
            mode: Clutter.AnimationMode.LINEAR,
        });
    }

    _showFnLock(on, detail) {
        this._only(this._lock);
        this._title.text = on ? 'Fn lock on' : 'Fn lock off';
        this._detail.text = detail;
        this._lock.icon_name = on ? 'changes-prevent-symbolic' : 'changes-allow-symbolic';
        this._lock.remove_all_transitions();
        this._lock.set_scale(1.4, 1.4);
        this._lock.ease({scale_x: 1, scale_y: 1, duration: 260, mode: Clutter.AnimationMode.EASE_OUT_BACK});
    }
}

export default class LegionctlOsd extends Extension {
    enable() {
        this._osd = new Osd();
        this._dbus = Gio.DBusExportedObject.wrapJSObject(IFACE, this);
        this._dbus.export(Gio.DBus.session, OBJECT_PATH);
        this._nameId = Gio.bus_own_name_on_connection(Gio.DBus.session, BUS_NAME, Gio.BusNameOwnerFlags.NONE, null, null);
    }

    disable() {
        Gio.bus_unown_name(this._nameId);
        this._dbus.unexport();
        this._osd.destroy();
        this._osd = null;
        this._dbus = null;
    }

    // D-Bus: org.legionctl.Osd.Show
    Show(kind, value, detail, _level) {
        this._osd.show(kind, value, detail);
    }
}
