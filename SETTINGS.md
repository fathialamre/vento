# Vento settings

Everything here is optional: the Settings window (Settings… in the panel under
the menu bar icon) covers the same options. This page is for reading or
editing the settings file by hand.

## The menu bar icon

Vento lives in the menu bar.

- **Left click** the icon to open the Quick panel: whether Vento is on or
  paused (with Pause or Resume), the Copy text, Clipboard, Pocket and Keep Awake
  tiles, Keep Awake's times, an Undo row when there is something to undo, and
  Settings… and Quit. It follows the Appearance setting. Settings… opens the
  Vento Settings window, whose pages are General (trigger, shake to open,
  terminal, Copy path format, Appearance), Wheel (layout, which actions show and
  in what order), Move (pinned and recent folders, the hold delay), Pocket
  (shortcut, remove after drag-out, keep a copy, whether it opens by kind or
  as a list), Clipboard (history on or off,
  its shortcut, Clear history), Copy from screen (its shortcut), Keep Awake (the
  times the Quick panel offers, what its tile starts, let the screen turn off)
  and About (the version, Open settings file). Changes
  apply and are saved at once.
- **Right click** for the menu: Copy from screen (macOS 14 or later), Open
  clipboard history, Open the Pocket, Start or Stop Keep Awake, Undo last
  move/copy/archive, Pause, Settings…, Open settings file, Quit.

## The settings file

`~/Library/Application Support/vento/settings.toml`

The Settings window writes this file; you can also edit it by hand, and Vento
picks up your edit when the Quick panel or the Settings window next opens or
changes something. All keys are optional; omitted keys use their defaults. If
the file cannot be parsed, or a value has the wrong type or a spelling not
listed below (for `layout`, `trigger` or `copy_path_format`, say), the whole
file is rejected: at startup all defaults apply and a warning is logged; while
Vento runs the current settings stay and the Quick panel shows the reason.
Unknown or duplicate action names are dropped individually. Values shown are
the defaults, except `terminal`, which is an example.

```toml
# "honeycomb" or "petals"
layout = "honeycomb"

# Modifier held while dragging: "ctrl", "alt", "shift" or "meta"
trigger = "ctrl"

# A quick back-and-forth shake of the pointer (any direction) while dragging
# files opens the wheel too
shake_to_open = true

# Actions shown on the wheel, in order: "pocket", "copy", "move", "copy_path",
# "terminal", "copy_to", "archive", "info", "convert", "pdf", "share" and
# "rename". A file that already lists `actions` must add "pocket", "copy_to",
# "archive", "info", "convert", "pdf", "share" or "rename" to show it. "pdf"
# works on macOS, Windows and Linux; "share" is macOS only, and is ignored
# elsewhere.
actions = ["pocket", "copy", "move", "copy_path", "terminal"]

# Actions turned off in the Settings window
hidden_actions = []

# Every action in the Wheel page's order, shown or hidden, so switching one
# on or off keeps its place. Written by the Settings window; ignored when the
# shown actions in it are not in the same order as `actions`
action_order = []

# Folders pinned to the Move and Copy to ring, in order; only the first 7 count
pinned_folders = []

# How long to hold on Move or Copy to before the ring opens, in milliseconds.
# Other values are rounded to the nearest 100 and kept between 200 and 1000.
move_hold_ms = 400

# "plain" or "shell_quoted" (single-quoted for POSIX shells)
copy_path_format = "plain"

# Language of the interface: "en" (English, the default), "ar" (Arabic, laid
# out right to left), "es" (Spanish), "fr" (French), "it" (Italian), "pt"
# (Portuguese, Portugal), "pt_BR" (Portuguese, Brazil), "tr" (Turkish) or
# "auto" (the first language in the system's preferred list that Vento has,
# else English). A code is matched on its language and region, then on its
# language part alone, so "AR" and "ar-LY" mean Arabic, "pt-BR" Brazilian and
# "pt-AO" European Portuguese; any other value means English and is logged,
# not rejected. A change applies at once, with no restart.
language = "en"

# "system" (follow the light or dark look of the system), "light" or "dark";
# the wheel, the Pocket and toasts stay dark either way
appearance = "system"

# Colours of the Quick panel and the Settings window: "vento", "github",
# "catppuccin", "rose_pine", "tokyo_night", "gruvbox" or "nord"
theme = "vento"

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

# Pocket: what the island's Pocket view opens on (the bottom bar switches
# between the two while it is open):
#   "kinds" - the items grouped by kind (images, video, audio, documents,
#             archives, folders, other), a row of tiles per kind
#   "list"  - every item, one row each
pocket_view = "kinds"

# Island: "virtual" (a drawn island at the top of the screen) or "classic" (the
# island that grows out of the notch). A new install starts on "virtual"
# through the Welcome setup; anything else without this key (a settings file
# written before it existed, or no settings file at all) keeps "classic" until
# you change it
island_mode = "virtual"

# Island: where the virtual island sits along the top edge: "left", "centre" or
# "right" ("center" is read as "centre")
island_place = "centre"

# Island: a free spot for the virtual island along the top edge, as a fraction
# of the screen's width (0 = the left end, 1 = the right end). Vento writes it
# when you drag the island and leave it between the left, centre and right
# snaps; while it is set, it wins over island_place. A value outside 0 to 1 is
# kept inside it; a value that is not a number counts as not set. Leave it out
# for no free spot
# island_place_free = 0.35

# Island: whether each screen keeps its own place for the virtual island (its
# own island_places entry below). With false, one place (island_place and
# island_place_free) holds on every screen
island_place_each_screen = true

# Island: the screen the virtual island is on, by its key: the screen's frame
# as "{width}x{height}+{x}+{y}" in whole pixels, as in the keys of
# island_places. Vento writes it when you drag the island onto another screen.
# Leave it out for the main screen (also used when this screen is not
# connected)
# island_screen = "2560x1440+1920+0"

# Island: the place of each screen is the [island_places] table at the end of
# this file (a table header must come after all the plain keys)

# Island: the virtual island's shape: "island" (a pill) or "circle" (a disc
# that stretches into a pill only to say something). Any other value is read
# as "island"
island_shape = "island"

# Island: how solid the virtual island is while resting, in percent (100 =
# solid). Other values are rounded to the nearest 5 and kept between 10 and 100
island_glass = 55

# Island: the shimmer the virtual island plays now and then while it rests:
# "off", "sweep" (a band of light crossing it), "edge_run" (a bright spot
# running round its edge), "halo" (the edge glowing softly), "sparkle" (a few
# tiny sparks) or "liquid" (the island stretches and settles). It never plays
# on the lip or the half-moon, or while the island is open, busy with a job or
# the news, paused, on battery saver, or after five minutes without the pointer
# moving or a click; after those, the next one waits a whole period. Nor in Do
# Not Disturb where Vento can tell (Windows: Focus assist, a presentation, a
# full-screen game; GNOME: notification banners off; a Mac's Focus is not
# read). Any other value is read as "sweep"
island_shimmer = "sweep"

# Island: how often the shimmer plays: "rarely" (every 10 minutes), "sometimes"
# (every 2 minutes) or "often" (every 30 seconds). Any other value is read as
# "sometimes"
island_shimmer_every = "sometimes"

# Island: play the shimmer only while the Pocket holds items
island_shimmer_needs_items = true

# Island: follow the system's Reduce motion setting. While it is on in the
# system, the island plays no shimmer and no shake, and uses plain fades
island_reduce_motion = true

# Island: show jobs in the island. On a Mac with a notch, and on GNOME, the
# island beside the notch (or in the top bar) shows a job running, its result
# and the news in its wings. With false the island stays as it was, and those
# notices appear as classic toasts. The virtual island ignores this
island_jobs = true

# Island: whether the one-time toast about the virtual island has been shown.
# Vento shows it once, on the classic island, where the virtual one can be
# chosen, then sets this to true; a new install never sees it
island_hint_shown = false

# Global shortcut that shows the Pocket: one or more of ctrl, alt, shift, meta
# (or cmd) joined with "+", then a single letter or digit, e.g. "cmd+shift+9"
pocket_shortcut = "ctrl+alt+p"

# Clipboard: record what you copy into the island's Clipboard history
clipboard_history = true

# Global shortcut that opens the island on the Clipboard history; same form
# as pocket_shortcut
clipboard_shortcut = "ctrl+alt+v"

# Screenshots: list the screenshots of the last 7 days in the island's
# Screenshots view (off until you turn it on)
screenshots = false

# Screenshots: show a card with the picture when a screenshot is saved
screenshot_card = true

# Screenshots: seconds the card stays after the pointer leaves it, from 2 to 8
screenshot_card_secs = 4

# Global shortcut for Copy from screen; same form as pocket_shortcut
# ("ctrl+alt+o" on Linux, where GNOME opens a terminal on Ctrl+Alt+T)
screen_text_shortcut = "ctrl+alt+t"

# Keep Awake: the times the Quick panel offers under its tile, in minutes
# (besides Until stopped and Until…). Each from 5 to 1440 (24 hours); they are
# sorted, repeats dropped and only the first 6 kept. An empty list leaves only
# Until stopped and Until…
keep_awake_times = [15, 30, 60, 120, 240, 480]

# Keep Awake: what a click on its tile, or Start Keep Awake in the menu,
# starts: one of keep_awake_times, in minutes, or 0 for until you stop it. A
# time not in keep_awake_times means 0
keep_awake_start = 0

# Keep Awake: let the screen dim, blank and lock as usual, keeping only the
# computer awake
keep_awake_screen_off = false

# PDF window, Images: the format last picked, "png" or "jpeg" (JPEG is
# drawn on white); any other value means "png"
pdf_image_format = "png"

# PDF window, Images: the quality last picked, in dots per inch: 300
# (Print) or 150 (Screen); any other value means 300
pdf_image_dpi = 300

# Settings window pages whose How to card you closed: any of "general",
# "wheel", "move", "pocket", "clipboard", "copy_from_screen", "keep_awake".
# The other pages open with it showing
closed_how_to = []

# true once the Welcome setup was finished, skipped or closed; Vento writes
# false only on a fresh install, and opens the setup while it is false
welcome_done = true

# Look for a new version on GitHub once a day (macOS). false stops the daily
# check; Settings → About → Check now still works
check_for_updates = true

# Rename window: the rules and order it opens on, written when you rename.
# last_rules lists the rules top to bottom, each with a kind ("replace",
# "add", "number", "case", "extension" or "template") and that kind's fields;
# a rule that does not read is dropped alone. A number rule's digits are kept
# between 1 and 6. order is "name", "modified" or "size" (anything else means
# "name"); reverse flips it
[rename]
last_rules = []
order = "name"
reverse = false

# Island: the place of each screen, by its key (see island_screen). Each entry
# has a "place" ("left", "centre" or "right") and, for a free spot, a "free"
# fraction as in island_place_free. Vento writes these when you drag the
# island; the entry of a screen that is not connected is kept. An entry that
# cannot be read is skipped. A table header must come after all the plain
# keys above: every key after it would belong to that table
# [island_places."1920x1080+0+0"]
# place = "left"
#
# [island_places."2560x1440+1920+0"]
# place = "centre"
# free = 0.25
```

Copy from screen needs macOS 14 or later; on macOS 13 the menu item and the
Copy from screen page are hidden and the shortcut is not registered. The
first use asks for Screen Recording; see the [README](README.md#copy-from-screen).

## Where Vento keeps its data

| What | Where |
|------|-------|
| Settings | `~/Library/Application Support/vento/settings.toml` |
| Recent Move and Copy to destinations | `~/Library/Application Support/vento/recent.json` |
| Pocket item list | `~/Library/Application Support/vento/pocket.json` |
| Pocket files Vento saved or kept | `~/Library/Application Support/vento/pocket/` |
| Clipboard history list | `~/Library/Application Support/vento/clipboard.json` |
| What each copy contained | `~/Library/Application Support/vento/clipboard/` |

Files Vento saves (non-file content) and keeps (copies, with
`pocket_keep_copies`) live in `~/Library/Application Support/vento/pocket/saved`
and `.../kept`; the item list is `~/Library/Application Support/vento/pocket.json`,
beside the `pocket/` folder. When such a Vento file leaves the Pocket it goes to
the Trash (after the Undo, or a minute after a drop), never deleted outright.
Your own files are only forgotten, never deleted, trashed or moved.

The clipboard history is stored **unencrypted**: anything you copy that Vento
does not skip (copies password managers mark as concealed or transient, and
copies made while Vento is paused or the history is off) is readable there until
it leaves the history. When a copy leaves (it falls off the 50-copy limit, you
remove it, or you clear the list and the Undo time passes), its folder in
`clipboard/` is deleted outright, not moved to the Trash. Pinned copies stay
until you remove them. Vento only ever deletes folders inside `clipboard/` and
never touches the files you copied. If `clipboard.json` cannot be read (damaged,
or written by a newer Vento), it is kept as `clipboard.json.bak` together with
the folders, and the history starts empty; emptying the history deletes that
backup too.

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
