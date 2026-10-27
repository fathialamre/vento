#!/bin/sh
# Builds vento in release mode and wraps it in target/bundle/Vento.app, with
# the app icon and LSUIElement so it stays out of the Dock. The languages in
# CFBundleLocalizations are the ones in lang/; add a language there too.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' crates/vento/Cargo.toml | head -n 1)
app=target/bundle/Vento.app

cargo build -p vento --release

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/vento "$app/Contents/MacOS/vento"
cp crates/vento/assets/vento.icns "$app/Contents/Resources/vento.icns"

cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleExecutable</key>
	<string>vento</string>
	<key>CFBundleIconFile</key>
	<string>vento</string>
	<key>CFBundleIdentifier</key>
	<string>io.github.fathialamre.vento</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleLocalizations</key>
	<array>
		<string>en</string>
		<string>ar</string>
	</array>
	<key>CFBundleName</key>
	<string>Vento</string>
	<key>CFBundleDisplayName</key>
	<string>Vento</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>$version</string>
	<key>CFBundleVersion</key>
	<string>$version</string>
	<key>LSMinimumSystemVersion</key>
	<string>13.0</string>
	<key>LSUIElement</key>
	<true/>
	<key>NSHighResolutionCapable</key>
	<true/>
</dict>
</plist>
EOF

# Ad-hoc signature, so Apple silicon runs the bundle as one signed unit.
codesign --force --sign - "$app"

echo "$app"
