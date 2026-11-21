#!/bin/sh
# Build "Oxide Commander.app" in dist/ (double-clickable, shows in Dock and Launchpad).
#
# Signing (certificates never leave this Mac; CI builds are signed ad hoc):
#   The first "Developer ID Application" identity in the keychain is used, with the
#   hardened runtime. A stable signature lets macOS remember the folder-access answers.
#   SIGN_IDENTITY="…" picks another identity; SIGN_IDENTITY=- forces ad-hoc signing.
#   Without any identity the app is signed ad hoc: macOS then treats every build as a
#   new app and asks for folder access again.
set -e
cd "$(dirname "$0")"
if [ -z "$SIGN_IDENTITY" ]; then
  SIGN_IDENTITY=$(security find-identity -v -p codesigning 2>/dev/null \
    | sed -n 's/.*"\(Developer ID Application:.*\)"/\1/p' | head -1)
fi
[ "$SIGN_IDENTITY" = "-" ] && SIGN_IDENTITY=""
cargo build --release
APP="dist/Oxide Commander.app"
# Version from Cargo.toml, so the app and the release tag always match.
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
BUILD=$(git rev-list --count HEAD 2>/dev/null || echo 0)
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/oxide-commander "$APP/Contents/MacOS/oxide-commander"
# The bundle identifier ties macOS privacy permissions to the app: never change it.
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Oxide Commander</string>
  <key>CFBundleDisplayName</key><string>Oxide Commander</string>
  <key>CFBundleIdentifier</key><string>io.github.cr34l.maccommander</string>
  <key>CFBundleExecutable</key><string>oxide-commander</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION.$BUILD</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSDesktopFolderUsageDescription</key><string>Oxide Commander shows and manages the files on your Desktop.</string>
  <key>NSDocumentsFolderUsageDescription</key><string>Oxide Commander shows and manages the files in your Documents folder.</string>
  <key>NSDownloadsFolderUsageDescription</key><string>Oxide Commander shows and manages the files in your Downloads folder.</string>
  <key>NSRemovableVolumesUsageDescription</key><string>Oxide Commander shows and manages the files on external drives.</string>
  <key>NSNetworkVolumesUsageDescription</key><string>Oxide Commander shows and manages the files on network shares.</string>
</dict></plist>
PLIST
if [ -n "$SIGN_IDENTITY" ]; then
  codesign --force --options runtime --timestamp -s "$SIGN_IDENTITY" "$APP"
  codesign --verify --strict "$APP"
  echo "Signed with: $SIGN_IDENTITY"
else
  codesign --force -s - "$APP" >/dev/null 2>&1 || true
fi
echo "Built $APP  (drag it to /Applications)"
