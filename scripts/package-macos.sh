#!/usr/bin/env bash
# Builds a universal (Apple Silicon + Intel) Detour.app in dist/. Run on a Mac.
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo build --release -p detour-app --target aarch64-apple-darwin
cargo build --release -p detour-app --target x86_64-apple-darwin

APP=dist/Detour.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create -output "$APP/Contents/MacOS/Detour" \
  target/aarch64-apple-darwin/release/detour-app \
  target/x86_64-apple-darwin/release/detour-app

# App icon: the binary can write its own shield artwork.
WORK=$(mktemp -d)
ICONSET="$WORK/Detour.iconset"
mkdir -p "$ICONSET"
"$APP/Contents/MacOS/Detour" --export-icon="$WORK/base.png"
for s in 16 32 128 256 512; do
  sips -z "$s" "$s" "$WORK/base.png" --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  sips -z $((s * 2)) $((s * 2)) "$WORK/base.png" --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/Detour.icns"
rm -rf "$WORK"

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Detour</string>
  <key>CFBundleDisplayName</key><string>Detour</string>
  <key>CFBundleIdentifier</key><string>com.detour.app</string>
  <key>CFBundleExecutable</key><string>Detour</string>
  <key>CFBundleIconFile</key><string>Detour</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
EOF

# Ad-hoc signature: enough to run locally. Distributing to other people needs a
# Developer ID signature and notarization; until then, right-click > Open.
cp assets/fonts/OFL.txt "$APP/Contents/Resources/PlusJakartaSans-LICENSE.txt"
cp LICENSE "$APP/Contents/Resources/LICENSE.txt"
codesign --force --deep --sign - "$APP"
echo "Built $APP"
