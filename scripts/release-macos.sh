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

(cd "$out" && ditto -c -k --norsrc --noextattr --noacl --keepParent "$name" "$zip")

echo "$out/$zip"
