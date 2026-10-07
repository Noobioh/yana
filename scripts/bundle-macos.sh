#!/bin/sh
# Builds dist/Yana.app: release binary, Info.plist, icon, ad-hoc signature.
set -eu
cd "$(dirname "$0")/.."

APP=dist/Yana.app
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

# BIN lets CI pass a prebuilt (e.g. universal) binary
if [ -z "${BIN:-}" ]; then
    cargo build --release
    BIN=target/release/ez-notes
fi

rm -rf "$APP" dist/Yana.iconset
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/Yana"

cargo run --quiet --release --example iconset -- dist/Yana.iconset
iconutil --convert icns dist/Yana.iconset --output "$APP/Contents/Resources/Yana.icns"
rm -rf dist/Yana.iconset

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Yana</string>
    <key>CFBundleDisplayName</key><string>Yana</string>
    <key>CFBundleIdentifier</key><string>app.yana.Yana</string>
    <key>CFBundleExecutable</key><string>Yana</string>
    <key>CFBundleIconFile</key><string>Yana</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# ad-hoc signature: enough to run on this Mac (Apple Silicon requires a signature)
codesign --force --deep --sign - "$APP"

echo "Built $APP ($VERSION)"
