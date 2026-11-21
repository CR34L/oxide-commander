#!/bin/sh
# Build "Mac Commander.app" in dist/ (double-clickable, shows in Dock and Launchpad).
set -e
cd "$(dirname "$0")"
cargo build --release
APP="dist/Mac Commander.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/mac-commander "$APP/Contents/MacOS/mac-commander"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Mac Commander</string>
  <key>CFBundleDisplayName</key><string>Mac Commander</string>
  <key>CFBundleIdentifier</key><string>local.maccommander</string>
  <key>CFBundleExecutable</key><string>mac-commander</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
codesign --force --deep -s - "$APP" >/dev/null 2>&1 || true
echo "Built $APP  (drag it to /Applications)"
