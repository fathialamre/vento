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

It also writes `target/release-zip/SHA256SUMS.txt` next to the zip. The in-app
updater checks the download against it, so each release uploads both files.

To publish a release:

1. Add a section for the version to both `CHANGELOG.md` and `CHANGELOG.ar.md`,
   with the same version and keys. Release notes and the What's new window come
   from them.
2. Tag the release (`vX.Y.Z`) and push the tag.
3. Build the Windows setup on a Windows PC (see [Windows](#windows)) and copy
   `target\release-setup\vento-X.Y.Z-windows-setup.exe` and its
   `SHA256SUMS.txt` to `target/release-setup/` on the Mac.
4. Build the zip, put both checksum lists in one `SHA256SUMS.txt` (the updater
   on each system looks for its own file's line), then create the release
   from the tag, with the changelog section as its notes:

```sh
scripts/release-macos.sh
cat target/release-zip/SHA256SUMS.txt target/release-setup/SHA256SUMS.txt >target/SHA256SUMS.txt
gh release create vX.Y.Z --verify-tag --notes-file <(scripts/release-notes.sh X.Y.Z) \
  target/release-zip/vento-X.Y.Z-macos-arm64.zip \
  target/release-setup/vento-X.Y.Z-windows-setup.exe target/SHA256SUMS.txt
```

A release without the setup, or without its line in `SHA256SUMS.txt`, offers
Windows no update; the same goes for the Mac and its zip.

`scripts/release-notes.sh X.Y.Z` prints that version's section of `CHANGELOG.md`
and fails if there is none. Do not mark the release as a draft or prerelease:
the updater ignores both, and ignores tags with a suffix such as `v0.4.0-beta`.

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
sudo apt install build-essential libfontconfig1-dev libxkbcommon-dev libglib2.0-dev libheif-dev
```

and running needs `libxkbcommon-x11-0` (`sudo apt install libxkbcommon-x11-0`).
Convert reads and writes HEIC with libheif's plugins; without them HEIC is
just not offered:

```sh
sudo apt install libheif-plugin-libde265 libheif-plugin-x265
```

To install Vento for your user (the app in `~/.local/bin`, its launcher and
icon, the extension, enabled, and a start at every login):

```sh
scripts/install-linux.sh
```

The release build links with fat LTO and needs about 7 GB of memory; on a
small machine without swap, add some first. The first time, log out and back
in once so GNOME Shell loads the extension; after a change to the extension,
log out again to load the new version. Vento then starts at login, or from the
app grid. The first launch asks you to allow its global shortcuts.
`scripts/install-linux.sh --no-autostart` installs it without the start at
login (GNOME's startup apps can also turn it off), and
`scripts/install-linux.sh --uninstall` removes it all, and keeps your settings
and data.

Global shortcuts go through the desktop portal, which only knows apps by their
`.desktop` file, so they work once Vento is installed. Started from a terminal
(`cargo run -p vento`), Vento belongs to that terminal's app, and GNOME may file
its shortcuts under that app's name.

Not there yet on Linux: Copy from screen.

## Windows

Vento runs on Windows 10 and 11, on x64 and ARM64 PCs. Building needs the
Visual Studio Build Tools with the C++ tools (and the Windows SDK, which brings
`rc.exe` for the app's icon); on an ARM64 PC, add the x64 tools as well to
build for x64. `cargo run -p vento` starts a debug build, with a console window
for its log; release builds have none.

To build the installer (one setup for x64 and ARM64 PCs, which installs for
the current user without administrator rights):

```powershell
rustup target add x86_64-pc-windows-msvc aarch64-pc-windows-msvc
winget install JRSoftware.InnoSetup
cargo install cargo-about --locked --features cli
.\scripts\release-windows.ps1
```

On an ARM64 PC, install cargo-about with `--target x86_64-pc-windows-msvc`:
its `ring` dependency needs clang to build for ARM64. The setup lands in
`target\release-setup\vento-<version>-windows-setup.exe`. It is signed with
Azure Trusted Signing when `VENTO_SIGN_DLIB` (the `Azure.CodeSigning.Dlib.dll`
from the `Microsoft.Trusted.Signing.Client` package) and `VENTO_SIGN_METADATA`
(its `metadata.json`) are set and you are signed in with `az login`; see the
top of the script. The script also writes the setup's line for the release's
`SHA256SUMS.txt` to `target\release-setup\SHA256SUMS.txt`.

An installed Vento updates itself with the setup: it downloads the release's
setup, checks it against `SHA256SUMS.txt` and quits; a copy of the old
`vento.exe` then runs the setup silently, keeping the folder and the tasks
chosen at the first install, and starts the new version. If the setup fails
or the new version does not start, the old `vento.exe` is put back. A Vento
that was not installed by the setup (a `cargo run` build, say) finds updates
but asks to be installed with the setup to take them. To try it against a
local test release, set `VENTO_UPDATE_API` to
the address of a latest-release JSON (plain `http` is allowed for
`127.0.0.1` and `localhost`).

A Vento started from inside a packaged (MSIX) app, such as a terminal in a
Store app, has its `%APPDATA%` redirected into that app's package, where other
apps cannot see the Pocket's files. Start it from Explorer, the Start menu or
a plain terminal to try it.

Not there yet on Windows: Copy from screen, PDF pages, Share, and HEIC in
Convert.
