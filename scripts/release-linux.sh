#!/bin/sh
# Builds the Ubuntu package published on GitHub, for this machine's
# architecture: target/debian/vento_<version>-1_<arch>.deb. Build it on the
# oldest Ubuntu it supports (24.04), as the release workflow does: the
# package then runs there and on every later one. Needs cargo-deb
# (cargo install cargo-deb --locked) and cargo-about (cargo install
# cargo-about --locked --features cli) for the third-party notices.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

# Picked up by the package's assets (crates/vento/Cargo.toml).
notices=target/deb-notices
rm -rf "$notices"
mkdir -p "$notices"
cp crates/vento/ui/icons/LICENSE-tabler "$notices/LICENSE-tabler"
# about.toml names the Mac's target; this machine's own is the one built.
host=$(rustc -vV | sed -n 's/^host: //p')
cargo about generate --fail --target "$host" about.hbs >"$notices/THIRD-PARTY-NOTICES.txt"

# cargo-about lists Rust crates only. libwebp-sys builds Google's libwebp C
# library into the app, so that library's own licence goes in as well.
webp=$(cargo metadata --format-version 1 --locked |
	grep -o '"manifest_path":"[^"]*/libwebp-sys-[^"/]*/Cargo.toml"' | head -n 1 |
	sed 's|^"manifest_path":"\(.*\)/Cargo.toml"$|\1|')
if [ -z "$webp" ] || [ ! -f "$webp/vendor/COPYING" ]; then
	echo "release-linux.sh: libwebp's COPYING not found" >&2
	exit 1
fi
{
	printf '\n--------------------------------------------------------------------------------\n'
	printf 'libwebp licence (BSD-3-Clause)\n\nUsed by:\n'
	printf '  - libwebp, the C library built into %s\n\n' "$(basename "$webp")"
	cat "$webp/vendor/COPYING"
} >>"$notices/THIRD-PARTY-NOTICES.txt"

cargo deb -p vento --locked
