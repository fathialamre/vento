# vento

A radial actions wheel for files. Start dragging a file, then press the
trigger modifier (it must be pressed after the drag has started) and a wheel
of actions appears under the cursor; release the mouse over an action to run
it. Releasing anywhere outside the wheel leaves the drop to whatever is
underneath. Esc cancels (if your macOS allows reading key state without extra
permission). Built with Rust and Slint.

vento is **macOS only for now**. The core and platform crates are checked
for Windows and Linux targets (the platform crate is a stub there, so the wheel
does nothing); full CI runs on all three systems once the repo has a remote.

Actions: Pocket, Copy, Move (with undo), Copy Path, Terminal.

## Requirements

- macOS 13 or later
- Rust via rustup: `rust-toolchain.toml` pins the toolchain (1.99.1) and
  rustup installs it on first build; the crates need at least 1.88.

Neither Accessibility nor Input Monitoring permission is required.

## Run

```sh
cargo run -p vento --release
```

vento lives in the menu bar; use the tray menu to pause it, undo the last move,
or reload settings.

## Settings

`~/Library/Application Support/vento/settings.toml`

All keys are optional; omitted keys use their defaults. If the file cannot be
parsed, or any value is outside the spellings listed below, the whole file is
rejected: at startup all defaults apply and a warning is logged; on
**Reload settings** the current settings stay and a toast shows the reason.
Unknown or duplicate action names are dropped individually. Values shown are the defaults, except
`terminal`, which is an example.

```toml
# "honeycomb" or "petals"
layout = "honeycomb"

# Modifier held while dragging: "ctrl", "alt", "shift" or "meta"
trigger = "ctrl"

# Actions shown on the wheel, in order: "pocket", "copy", "move", "copy_path",
# "terminal". A file that already lists `actions` must add "pocket" to show it.
actions = ["pocket", "copy", "move", "copy_path", "terminal"]

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
