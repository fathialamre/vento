# Building vento

Most people should download vento from the
[Releases page](https://github.com/fathialamre/vento/releases/latest). Build it
yourself if you want to change it, or if you have an Intel Mac (the release is
Apple Silicon only for now).

## Requirements

- macOS 13 or later
- Rust via [rustup](https://rustup.rs): `rust-toolchain.toml` pins the toolchain
  (1.99.1) and rustup installs it on first build; the crates need at least 1.93.

## Run from source

```sh
cargo run -p vento --release
```

## Build the app

```sh
scripts/bundle-macos.sh
open target/bundle/vento.app
```

This builds `target/bundle/vento.app`, which carries the app icon and stays out of
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

## Tests

```sh
cargo test --workspace
```

## Other systems

vento is macOS only for now. The core and platform crates are checked for
Windows and Linux targets (the platform crate is a stub there, so the wheel does
nothing), and CI builds and tests on all three.
