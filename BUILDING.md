# Building and working on Vento

Most people should download Vento from the
[Releases page](https://github.com/fathialamre/vento/releases/latest). Build it
yourself if you want to add features or fix something, or if you have an Intel Mac
(the release is Apple Silicon only for now).

## Requirements

- macOS 13 or later, or Linux with GNOME on Wayland (see [Linux](#linux))
- Rust via [rustup](https://rustup.rs): `rust-toolchain.toml` pins the toolchain
  (1.99.1) and rustup installs it on first build; the crates need at least 1.93.

## Run from source

```sh
git clone https://github.com/fathialamre/vento.git
cd vento
cargo run -p vento --release
```

Quit any other copy of Vento first, so two wheels don't open at once.

## How the code is laid out

| Crate | What lives there |
|-------|------------------|
| `crates/vento-core` | Everything that does not need the operating system: the drag trigger, wheel and ring geometry, actions, settings, file moves and copies with undo, archives, the Pocket's data, folder sizes. Pure Rust with unit tests. |
| `crates/vento-platform` | Talking to the system behind one `Platform` trait: global mouse and key state, screens, clipboard, terminals, the folder picker, the Trash. `macos/` and `linux/` are the real implementations; `stub.rs` stands in on other systems. |
| `gnome-shell/` | The GNOME Shell extension Vento needs on Linux: it tells Vento the pointer, drags, screens and clipboard, which GNOME's Wayland hides from apps. |
| `crates/vento` | The app: the menu bar icon, the wheel and every window (Slint files in `crates/vento/ui/`), and the glue that runs actions. |

A new action usually means an `ActionId` in `vento-core/src/actions.rs`, its work
in `vento/src/exec.rs`, and an icon in `crates/vento/ui/icons/`. Anything that
needs the system goes through `Platform`, so `vento-core` stays testable.

## Build the app

```sh
scripts/bundle-macos.sh
open target/bundle/Vento.app
```

This builds `target/bundle/Vento.app`, which carries the app icon and stays out of
the Dock. Copy it to `/Applications` to keep it.

## Build the release zip

```sh
scripts/release-macos.sh
```

This builds the app and zips it with the licence files into
`target/release-zip/vento-<version>-macos-arm64.zip`, the file published on the
Releases page. It needs `cargo-about` for the third-party notices:

```sh
cargo install cargo-about --locked --features cli
```

## Before you send a change

CI runs these on macOS, Linux and Windows; run them locally first:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Linux

Vento runs on GNOME on Wayland (built and tried on Ubuntu 26.04). Wayland lets
no app see the pointer, drags in other apps or the clipboard, or place its own
windows, so Vento comes in two parts:

- the app, whose windows are X11 windows under XWayland, so the wheel and the
  Pocket island can sit exactly under the cursor, above every window;
- a GNOME Shell extension (`gnome-shell/vento@fathialamre.github.io`) that
  answers the app over the session bus.

Building needs a few development packages:

```sh
sudo apt install build-essential libfontconfig1-dev libxkbcommon-dev libglib2.0-dev
```

and running needs `libxkbcommon-x11-0` (`sudo apt install libxkbcommon-x11-0`).

To install Vento for your user (the app in `~/.local/bin`, its launcher and
icon, and the extension, enabled):

```sh
scripts/install-linux.sh
```

The first time, log out and back in once so GNOME Shell loads the extension;
after a change to the extension, log out again to load the new version. Then
start Vento from the app grid. The first launch asks you to allow its global
shortcuts. `scripts/install-linux.sh --uninstall` removes it all, and keeps
your settings and data.

Global shortcuts go through the desktop portal, which only knows apps by their
`.desktop` file, so they work once Vento is installed. Started from a terminal
(`cargo run -p vento`), Vento belongs to that terminal's app, and GNOME may file
its shortcuts under that app's name.

Not there yet on Linux: dragging out of the Pocket, dropping text or links on
the island, the item menu in the Pocket, Copy from screen and launch at login.

## Windows

`vento-core` builds and passes its tests on Windows. What is missing is a
`Platform` implementation in `vento-platform` (mouse and key state, the dragged
files, overlay windows for the wheel, the Pocket's drop target, clipboard,
terminals, the folder picker, the Trash) and the app's windows; until then
`stub.rs` stands in. If you want to help, `vento-platform/src/lib.rs` lists
everything a new system needs.
