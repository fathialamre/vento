# Building and working on Vento

Most people should download Vento from the
[Releases page](https://github.com/fathialamre/vento/releases/latest). Build it
yourself if you want to add features or fix something, or if you have an Intel Mac
(the release is Apple Silicon only for now).

## Requirements

- macOS 13 or later
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
| `crates/vento-platform` | Talking to the system behind one `Platform` trait: global mouse and key state, screens, clipboard, terminals, the folder picker, the Trash. `macos/` is the real implementation; `stub.rs` stands in on other systems. |
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

## Linux and Windows

The released app is macOS only for now; Linux and Windows support is in progress.
`vento-core` already builds and passes its tests on both. What is missing is a
`Platform` implementation for each in `vento-platform` (mouse and key state, the
dragged files, overlay windows for the wheel, the Pocket's drop target,
clipboard, terminals, the folder picker, the Trash), and the app in
`crates/vento` is only built for macOS so far. Until then `stub.rs` stands in. If
you want to help, `vento-platform/src/lib.rs` lists everything a new system
needs.
