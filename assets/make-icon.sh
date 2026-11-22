#!/bin/sh
# Regenerate the app icon from assets/icon.svg (needs rsvg-convert: brew install librsvg):
#   AppIcon.icns  macOS bundle icon (bundle-macos.sh copies it into the app)
#   icon.png      512 px, compiled into the binary as the window / Dock icon
set -e
cd "$(dirname "$0")"
SET=$(mktemp -d)/AppIcon.iconset
mkdir -p "$SET"
for s in 16 32 128 256 512; do
  rsvg-convert -w $s -h $s icon.svg -o "$SET/icon_${s}x${s}.png"
  rsvg-convert -w $((s * 2)) -h $((s * 2)) icon.svg -o "$SET/icon_${s}x${s}@2x.png"
done
iconutil -c icns "$SET" -o AppIcon.icns
cp "$SET/icon_512x512.png" icon.png
rm -rf "$(dirname "$SET")"
echo "Wrote assets/AppIcon.icns and assets/icon.png"
