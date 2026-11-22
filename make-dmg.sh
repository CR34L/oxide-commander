#!/bin/sh
# Build the installer disk image dist/Oxide-Commander-<version>-macos-<arch>.dmg:
# "Oxide Commander.app" next to an Applications shortcut, to drag it over.
#
# The app is built (and signed) by bundle-macos.sh; the image is signed with the same
# identity when there is one (SIGN_IDENTITY works the same way).
set -e
cd "$(dirname "$0")"
./bundle-macos.sh
if [ -z "$SIGN_IDENTITY" ]; then
  SIGN_IDENTITY=$(security find-identity -v -p codesigning 2>/dev/null \
    | sed -n 's/.*"\(Developer ID Application:.*\)"/\1/p' | head -1)
fi
[ "$SIGN_IDENTITY" = "-" ] && SIGN_IDENTITY=""
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ARCH=$(uname -m)
DMG="dist/Oxide-Commander-$VERSION-macos-$ARCH.dmg"
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT
ditto "dist/Oxide Commander.app" "$STAGE/Oxide Commander.app"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
hdiutil create -volname "Oxide Commander $VERSION" -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$DMG" >/dev/null
if [ -n "$SIGN_IDENTITY" ]; then
  codesign --force --timestamp -s "$SIGN_IDENTITY" "$DMG"
  echo "Signed with: $SIGN_IDENTITY"
fi
hdiutil verify "$DMG" >/dev/null
echo "Built $DMG"
