#!/bin/sh
# Builds the release zip published on GitHub: Vento.app plus the licence files,
# in target/release-zip/vento-<version>-macos-arm64.zip. Needs cargo-about
# (cargo install cargo-about --locked --features cli) for the third-party notices.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

if [ "$(uname -m)" != "arm64" ]; then
	echo "release-macos.sh: build the release on an Apple Silicon Mac" >&2
	exit 1
fi

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' crates/vento/Cargo.toml | head -n 1)
name="vento-$version"
out=target/release-zip
stage="$out/$name"
zip="$name-macos-arm64.zip"

scripts/bundle-macos.sh >/dev/null

rm -rf "$stage" "$out/$zip"
mkdir -p "$stage"
ditto target/bundle/Vento.app "$stage/Vento.app"
cp LICENSE "$stage/LICENSE"
cp crates/vento/ui/icons/LICENSE-tabler "$stage/LICENSE-tabler"
cargo about generate --fail about.hbs >"$stage/THIRD-PARTY-NOTICES.txt"

# cargo-about lists Rust crates only. libwebp-sys builds Google's libwebp C
# library into the app, so that library's own licence goes in as well.
webp=$(cargo metadata --format-version 1 --locked |
	grep -o '"manifest_path":"[^"]*/libwebp-sys-[^"/]*/Cargo.toml"' | head -n 1 |
	sed 's|^"manifest_path":"\(.*\)/Cargo.toml"$|\1|')
if [ -z "$webp" ] || [ ! -f "$webp/vendor/COPYING" ]; then
	echo "release-macos.sh: libwebp's COPYING not found" >&2
	exit 1
fi
{
	printf '\n--------------------------------------------------------------------------------\n'
	printf 'libwebp licence (BSD-3-Clause)\n\nUsed by:\n'
	printf '  - libwebp, the C library built into %s\n\n' "$(basename "$webp")"
	cat "$webp/vendor/COPYING"
} >>"$stage/THIRD-PARTY-NOTICES.txt"

# Crash reports from this build are symbolicated with its dSYM; keep it next
# to the zip (it is not published).
rm -rf "$out/$name.dSYM"
ditto target/release/vento.dSYM "$out/$name.dSYM"

(cd "$out" && ditto -c -k --norsrc --noextattr --noacl --keepParent "$name" "$zip")

echo "$out/$zip"
