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

# The disk image people download: the same files, a link to Applications to
# drag the app onto, and the app icon on the volume. The updater keeps using
# the zip.
dmg="$name-macos-arm64.dmg"
dmgstage="$out/dmg"
rm -rf "$dmgstage" "$out/$dmg"
mkdir -p "$dmgstage"
ditto "$stage" "$dmgstage"
ln -s /Applications "$dmgstage/Applications"
cp crates/vento/assets/vento.icns "$dmgstage/.VolumeIcon.icns"
# The volume's custom-icon flag only sticks when set on the mounted volume,
# so the image is made writable first and compressed after.
rw="$out/$name-rw.dmg"
rm -f "$rw"
hdiutil create -quiet -volname "Vento $version" -srcfolder "$dmgstage" -fs HFS+ -format UDRW "$rw"
mount=$(mktemp -d)
hdiutil attach -quiet -nobrowse -noautoopen -mountpoint "$mount" "$rw"
SetFile -a C "$mount"
hdiutil detach -quiet "$mount"
rmdir "$mount"
hdiutil convert -quiet "$rw" -format UDZO -imagekey zlib-level=9 -o "$out/$dmg"
rm -rf "$dmgstage" "$rw"

# Checksums the in-app updater verifies the download against; published next
# to the zip and the disk image.
(cd "$out" && shasum -a 256 "$zip" "$dmg" >SHA256SUMS.txt)

echo "$out/$zip"
echo "$out/$dmg"
echo "$out/SHA256SUMS.txt"
