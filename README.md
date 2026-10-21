# vento

A radial actions wheel for files. Start dragging a file, then press the
trigger modifier (it must be pressed after the drag has started) and a wheel
of actions appears under the cursor; release the mouse over an action to run
it. Releasing anywhere outside the wheel leaves the drop to whatever is
underneath. Esc cancels (if your macOS allows reading key state without extra
permission).

vento is **macOS only for now**. The core and platform crates are checked
for Windows and Linux targets (the platform crate is a stub there, so the wheel
does nothing); full CI runs on all three systems once the repo has a remote.

Actions: Pocket, Copy, Move (with undo), Copy to (with undo), Archive (with
undo, off by default), Copy path, Terminal.

## Move and Copy to

Release on **Move** (or **Copy to**) and a folder picker opens. To skip the
picker, hold the pointer on the action for a moment (400 ms by default) and a
ring of folders opens around it: the five most recent destinations first, then
your pinned folders. Release on a folder to move (or copy) the files straight
there. Folders that no longer exist are left out of the ring.

Both actions remember where files went. **Undo last move** (or **Undo last
copy**) in the menu puts the files back; undoing a copy only trashes copies
vento made that have not changed since.

Pin the folders you use most in the settings panel's Move tab with **Add
folder**, or pin one from the recent list; up to seven fit. The same tab sets how
long the hold takes (200 to 1000 ms) and clears the recent list.

## Archive

Release on **Archive** and vento compresses or extracts, depending on what you
dragged. If every item is a zip, rar or 7z archive, each one is extracted;
anything else, including a mix, is compressed into one zip. An item counts as
an archive only when both its extension and its contents say so, so dropping a
`.docx` or `.xlsx` zips it rather than unpacking it.

- **Compress** writes a zip next to the first item: `Report.docx` gives
  `Report.zip`, several items give `Archive.zip`. Folders are stored whole,
  without `.DS_Store` or a `__MACOSX` folder.
- **Extract** reads zip, rar and 7z (including multi-volume rar sets) and puts
  the contents next to the archive, in a folder named after it (or, like
  Finder, directly if the archive holds one top-level item). Files from a
  downloaded archive keep its quarantine mark, so Gatekeeper still checks them.
- **rar is extract-only**: the RAR format's licence forbids writing it.
- **Passwords**: an encrypted archive opens a small window asking for the
  password; a wrong one keeps it open, Cancel skips that archive and the rest of
  the drop carries on. Passwords are never saved or logged.
- **Progress**: a job that runs longer than half a second shows a progress
  window with a **Cancel** button. Cancelling removes what vento was still
  writing; in a drop of several archives, the ones already extracted stay.
- Nothing is ever overwritten (`Archive (2).zip`), and a result only appears
  when it is complete.
- **Undo last archive** (toast or menu) moves what vento created to the Trash,
  unless you have changed it since.

Archive is off by default: switch it on in the settings panel's Wheel tab, where
it is listed with the hidden actions.

## Requirements

- macOS 13 or later
- Rust via rustup: `rust-toolchain.toml` pins the toolchain (1.99.1) and
  rustup installs it on first build; the crates need at least 1.93.

Neither Accessibility nor Input Monitoring permission is required.

## Run

```sh
cargo run -p vento --release
```

Or build `target/bundle/vento.app`, which carries the app icon and stays out
of the Dock:

```sh
scripts/bundle-macos.sh
open target/bundle/vento.app
```

vento lives in the menu bar.

- **Left click** the icon to open the settings panel. Its tabs are Wheel
  (layout, which actions show and in what order), Move (pinned and recent
  folders, the hold delay), Pocket (shortcut, remove after drag-out, keep a
  copy) and General (trigger, terminal, Copy path format); Pause and Quit are at
  the bottom. It follows the system light or dark appearance. Changes apply and
  are saved at once.
- **Right click** for the menu: Undo last move/copy/archive, Pause, Open
  settings file, Quit.

## Settings

`~/Library/Application Support/vento/settings.toml`

The settings panel writes this file; you can also edit it by hand, and vento
picks up your edit when the panel next opens or changes something. All keys are
optional; omitted keys use their defaults. If the file cannot be parsed, or a
value has the wrong type or a spelling not listed below (for `layout`,
`trigger` or `copy_path_format`, say), the whole file is rejected: at
startup all defaults apply and a warning is logged; while vento runs the current
settings stay and the panel shows the reason. Unknown or duplicate action names
are dropped individually. Values shown are the defaults, except `terminal`,
which is an example.

```toml
# "honeycomb" or "petals"
layout = "honeycomb"

# Modifier held while dragging: "ctrl", "alt", "shift" or "meta"
trigger = "ctrl"

# Actions shown on the wheel, in order: "pocket", "copy", "move", "copy_path",
# "terminal", "copy_to", "archive". A file that already lists `actions` must add
# "pocket", "copy_to" or "archive" to show it.
actions = ["pocket", "copy", "move", "copy_path", "terminal"]

# Actions turned off in the panel (the panel lists them below the shown ones)
hidden_actions = []

# Folders pinned to the Move and Copy to ring, in order; only the first 7 count
pinned_folders = []

# How long to hold on Move or Copy to before the ring opens, in milliseconds.
# Other values are rounded to the nearest 100 and kept between 200 and 1000.
move_hold_ms = 400

# "plain" or "shell_quoted" (single-quoted for POSIX shells)
copy_path_format = "plain"

# Bundle id of the terminal app to open; the default (and the fallback when
# the app is not installed) is Terminal.app
# terminal = "com.googlecode.iterm2"

# Pocket: after a successful drag-out, remove the items from the Pocket (files
# vento saved or kept go to the Trash a minute later; references to your own
# files are just forgotten)
pocket_remove_after_drop = false

# Pocket: keep a copy of each file inside vento's own data folder instead of
# only a reference to the original
pocket_keep_copies = false

# Global shortcut that shows the Pocket: one or more of ctrl, alt, shift, meta
# (or cmd) joined with "+", then a single letter or digit, e.g. "cmd+shift+9"
pocket_shortcut = "ctrl+alt+p"
```

## Pocket

The Pocket is a holding shelf for files, text, links and images you are
carrying between places. With items in it, a small island shows at the top of
the screen by the notch; it is hidden while empty.

Add to it by any of these:

- the **Pocket** action on the wheel;
- dropping onto the island near the notch;
- shaking while dragging;
- the shortcut, ⌃⌥P by default.

Take items out by dragging one to any app. Click items to select them, then drag
any selected item to take the whole selection; **Drag all** takes every item.
The x on an item removes it and **Clear** empties the Pocket; both offer Undo.

Settings keys (see above): `pocket_shortcut`, `pocket_keep_copies`,
`pocket_remove_after_drop`. A settings file that already lists `actions` must
add `"pocket"` to show the action.

Files vento saves (non-file content) and keeps (copies, with
`pocket_keep_copies`) live in `~/Library/Application Support/vento/pocket/saved`
and `.../kept`; the item list is `~/Library/Application Support/vento/pocket.json`,
beside the `pocket/` folder. When such a vento file leaves the Pocket it goes to
the Trash (after the Undo, or a minute after a drop), never deleted outright.
Your own files are only forgotten, never deleted, trashed or moved.

## License

GPL-3.0-only. See [LICENSE](LICENSE).

Wheel icons are from [Tabler Icons](https://tabler.io/icons) 3.49.0, MIT-licensed (see
[crates/vento/ui/icons/LICENSE-tabler](crates/vento/ui/icons/LICENSE-tabler)).
