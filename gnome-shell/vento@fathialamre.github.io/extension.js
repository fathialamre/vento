// Vento's helper inside GNOME Shell.
//
// On Wayland an app cannot see the global pointer, drags in other apps or
// the clipboard without focus. This extension can, and answers Vento over
// the session bus as org.vento.Shell1. Vento polls it; nothing here sends
// signals, so every change is a counter Vento compares.

import GLib from 'gi://GLib';
import Gio from 'gi://Gio';
import Meta from 'gi://Meta';
import St from 'gi://St';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const BUS_NAME = 'org.vento.Shell1';
const OBJECT_PATH = '/org/vento/Shell1';

// Raised when the interface changes in a way Vento must know about.
const API_VERSION = 2;

// The room kept in the top bar for the Pocket island: its two wings and the
// gap between them.
const SLOT_WIDTH = 96;

// Clutter.ModifierType.BUTTON1_MASK.
const BUTTON1 = 1 << 8;

const FILE_TYPES = ['text/uri-list', 'x-special/gnome-copied-files'];

const IFACE = `
<node>
  <interface name="org.vento.Shell1">
    <method name="Version">
      <arg type="u" direction="out" name="version"/>
    </method>
    <!-- x, y, modifier and button mask, drag serial, drag active,
         drag has files, drag cancelled, clipboard serial, monitors serial -->
    <method name="State">
      <arg type="(iiuubbbuu)" direction="out" name="state"/>
    </method>
    <!-- Per monitor, primary first: frame x y w h, work area x y w h, scale -->
    <method name="Monitors">
      <arg type="a(iiiiiiiid)" direction="out" name="monitors"/>
    </method>
    <method name="DragUris">
      <arg type="as" direction="out" name="uris"/>
    </method>
    <method name="ClipboardTypes">
      <arg type="as" direction="out" name="types"/>
    </method>
    <method name="ReadClipboard">
      <arg type="s" direction="in" name="mimetype"/>
      <arg type="ay" direction="out" name="data"/>
    </method>
    <!-- Focus the top window of process pid, remembering the focused one -->
    <method name="Activate">
      <arg type="u" direction="in" name="pid"/>
      <arg type="b" direction="out" name="done"/>
    </method>
    <!-- Give focus back to the remembered window if pid still has it -->
    <method name="Deactivate">
      <arg type="u" direction="in" name="pid"/>
      <arg type="b" direction="out" name="done"/>
    </method>
    <!-- Keep room in the top bar for the Pocket island, after the clock,
         while the caller stays on the bus -->
    <method name="ReserveIsland">
      <arg type="b" direction="in" name="reserve"/>
    </method>
    <!-- Whether the room is there, and its x y w h; it moving bumps the
         monitors serial -->
    <method name="IslandSlot">
      <arg type="(biiii)" direction="out" name="slot"/>
    </method>
  </interface>
</node>`;

/** Reads `type` of `selectionType` into a GLib.Bytes, then calls `done`. */
function readSelection(selectionType, type, done) {
    const stream = Gio.MemoryOutputStream.new_resizable();
    const selection = global.display.get_selection();
    selection.transfer_async(selectionType, type, -1, stream, null, (sel, res) => {
        try {
            sel.transfer_finish(res);
            stream.close(null);
            done(stream.steal_as_bytes());
        } catch (e) {
            console.warn(`Vento: reading ${type} failed: ${e.message}`);
            done(null);
        }
    });
}

class Service {
    constructor() {
        this._dragSerial = 0;
        this._dragActive = false;
        this._dragHasFiles = false;
        this._dragCancelled = false;
        this._clipSerial = 0;
        this._monitorsSerial = 0;
        this._previousWindow = null;
        this._slot = null;
        this._slotWatch = 0;
    }

    Version() {
        return API_VERSION;
    }

    State() {
        const [x, y, mods] = global.get_pointer();
        return [x, y, mods, this._dragSerial, this._dragActive,
            this._dragHasFiles, this._dragCancelled, this._clipSerial,
            this._monitorsSerial];
    }

    Monitors() {
        const display = global.display;
        const workspace = global.workspace_manager.get_active_workspace();
        const out = [];
        for (let i = 0; i < display.get_n_monitors(); i++) {
            const g = display.get_monitor_geometry(i);
            const w = workspace.get_work_area_for_monitor(i);
            out.push([g.x, g.y, g.width, g.height, w.x, w.y, w.width, w.height,
                display.get_monitor_scale(i)]);
        }
        const primary = display.get_primary_monitor();
        if (primary > 0 && primary < out.length)
            out.unshift(out.splice(primary, 1)[0]);
        return out;
    }

    DragUrisAsync(_params, invocation) {
        if (!this._dragActive || !this._dragHasFiles) {
            invocation.return_value(new GLib.Variant('(as)', [[]]));
            return;
        }
        readSelection(Meta.SelectionType.SELECTION_DND, 'text/uri-list', bytes => {
            const text = bytes ? new TextDecoder().decode(bytes.toArray()) : '';
            const uris = text.split(/\r?\n/).filter(l => l && !l.startsWith('#'));
            invocation.return_value(new GLib.Variant('(as)', [uris]));
        });
    }

    ClipboardTypes() {
        return global.display.get_selection()
            .get_mimetypes(Meta.SelectionType.SELECTION_CLIPBOARD);
    }

    ReadClipboardAsync([type], invocation) {
        readSelection(Meta.SelectionType.SELECTION_CLIPBOARD, type, bytes => {
            const data = bytes ? bytes.toArray() : new Uint8Array();
            invocation.return_value(new GLib.Variant('(ay)', [data]));
        });
    }

    Activate(pid) {
        const focus = global.display.get_focus_window();
        if (focus && focus.get_pid() !== pid)
            this._previousWindow = focus;
        const mine = global.display.get_tab_list(Meta.TabList.NORMAL_ALL, null)
            .find(w => w.get_pid() === pid);
        if (!mine)
            return false;
        Main.activateWindow(mine);
        return true;
    }

    Deactivate(pid) {
        const previous = this._previousWindow;
        this._previousWindow = null;
        const focus = global.display.get_focus_window();
        if (!previous || !focus || focus.get_pid() !== pid)
            return false;
        // A window that closed meanwhile has no compositor-side actor.
        if (!previous.get_compositor_private())
            return false;
        Main.activateWindow(previous);
        return true;
    }

    ReserveIslandAsync([reserve], invocation) {
        if (reserve)
            this._reserve(invocation.get_sender());
        else
            this.release();
        invocation.return_value(null);
    }

    IslandSlot() {
        const slot = this._slot;
        if (!slot || !slot.get_stage())
            return [false, 0, 0, 0, 0];
        const [x, y] = slot.get_transformed_position();
        const [w, h] = slot.get_transformed_size();
        return [true, Math.round(x), Math.round(y), Math.round(w), Math.round(h)];
    }

    /** Puts the room after the clock, for as long as `owner` is on the bus. */
    _reserve(owner) {
        if (this._slot)
            return;
        const slot = new St.Widget({width: SLOT_WIDTH, y_expand: true, reactive: false});
        const box = Main.panel._centerBox;
        const clock = Main.panel.statusArea.dateMenu?.container;
        if (clock && clock.get_parent() === box)
            box.insert_child_above(slot, clock);
        else
            box.add_child(slot);
        // The clock's width changes with the time: Vento moves the island.
        slot.connectObject('notify::allocation', () => this.monitorsChanged(), this);
        this._slot = slot;
        this._slotWatch = Gio.bus_watch_name(Gio.BusType.SESSION, owner,
            Gio.BusNameWatcherFlags.NONE, null, () => this.release());
        this.monitorsChanged();
    }

    /** Gives the room back. */
    release() {
        if (this._slotWatch) {
            Gio.bus_unwatch_name(this._slotWatch);
            this._slotWatch = 0;
        }
        if (this._slot) {
            this._slot.disconnectObject(this);
            this._slot.destroy();
            this._slot = null;
            this.monitorsChanged();
        }
    }

    dragStarted() {
        const types = global.display.get_selection()
            .get_mimetypes(Meta.SelectionType.SELECTION_DND);
        this._dragSerial++;
        this._dragActive = true;
        this._dragHasFiles = types.some(t => FILE_TYPES.includes(t));
        this._dragCancelled = false;
    }

    dragEnded() {
        // A drag that ends with the button still held was cancelled
        // (Escape), not dropped.
        const [, , mods] = global.get_pointer();
        this._dragActive = false;
        this._dragCancelled = (mods & BUTTON1) !== 0;
    }

    selectionChanged(type) {
        if (type === Meta.SelectionType.SELECTION_CLIPBOARD)
            this._clipSerial++;
    }

    monitorsChanged() {
        this._monitorsSerial++;
    }
}

export default class VentoExtension extends Extension {
    enable() {
        this._service = new Service();
        this._dbus = Gio.DBusExportedObject.wrapJSObject(IFACE, this._service);
        this._dbus.export(Gio.DBus.session, OBJECT_PATH);
        this._nameId = Gio.bus_own_name(Gio.BusType.SESSION, BUS_NAME,
            Gio.BusNameOwnerFlags.NONE, null, null, null);

        const service = this._service;
        global.backend.get_dnd().connectObject(
            'dnd-enter', () => service.dragStarted(),
            'dnd-leave', () => service.dragEnded(),
            this);
        global.display.get_selection().connectObject(
            'owner-changed', (_sel, type) => service.selectionChanged(type),
            this);
        Main.layoutManager.connectObject(
            'monitors-changed', () => service.monitorsChanged(),
            this);
        global.display.connectObject(
            'workareas-changed', () => service.monitorsChanged(),
            this);
    }

    disable() {
        global.backend.get_dnd().disconnectObject(this);
        global.display.get_selection().disconnectObject(this);
        Main.layoutManager.disconnectObject(this);
        global.display.disconnectObject(this);
        this._service.release();
        Gio.bus_unown_name(this._nameId);
        this._dbus.unexport();
        this._dbus = null;
        this._service = null;
    }
}
