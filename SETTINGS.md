# vento settings

Everything here is optional: the settings panel under the menu bar icon covers
the same options. This page is for reading or editing the settings file by hand.

## The menu bar icon

vento lives in the menu bar.

- **Left click** the icon to open the settings panel. Its tabs are Wheel
  (layout, which actions show and in what order), Move (pinned and recent
  folders, the hold delay), Pocket (shortcut, remove after drag-out, keep a
  copy) and General (trigger, terminal, Copy path format); Pause and Quit are at
  the bottom. It follows the system light or dark appearance. Changes apply and
  are saved at once.
- **Right click** for the menu: Undo last move/copy/archive, Pause, Open
  settings file, Quit.

## The settings file

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
# "terminal", "copy_to", "archive", "info". A file that already lists `actions`
# must add "pocket", "copy_to", "archive" or "info" to show it.
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

## Where vento keeps its data

| What | Where |
|------|-------|
| Settings | `~/Library/Application Support/vento/settings.toml` |
| Recent Move and Copy to destinations | `~/Library/Application Support/vento/recent.json` |
| Pocket item list | `~/Library/Application Support/vento/pocket.json` |
| Pocket files vento saved or kept | `~/Library/Application Support/vento/pocket/` |

Files vento saves (non-file content) and keeps (copies, with
`pocket_keep_copies`) live in `~/Library/Application Support/vento/pocket/saved`
and `.../kept`; the item list is `~/Library/Application Support/vento/pocket.json`,
beside the `pocket/` folder. When such a vento file leaves the Pocket it goes to
the Trash (after the Undo, or a minute after a drop), never deleted outright.
Your own files are only forgotten, never deleted, trashed or moved.

## Archive details

- An item counts as an archive only when both its extension and its contents
  say so, so dropping a `.docx` or `.xlsx` zips it rather than unpacking it.
- Extract reads zip, rar and 7z, including multi-volume rar sets. rar is
  extract-only: the RAR format's licence forbids writing it.
- Files from a downloaded archive keep its quarantine mark, so Gatekeeper still
  checks them.
- Compressed folders are stored whole, without `.DS_Store` or a `__MACOSX`
  folder.
- Passwords for encrypted archives are never saved or logged.
