#!/usr/bin/env bash
# Gera dist/Jarvis_Core-<versão>-macos-universal.dmg e um .tar.gz com a CLI.
# Deve rodar em macOS (usa lipo, sips, iconutil e hdiutil, que vêm com o sistema/Xcode CLT).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
mkdir -p dist
rustup target add aarch64-apple-darwin x86_64-apple-darwin
for T in aarch64-apple-darwin x86_64-apple-darwin; do
  MACOSX_DEPLOYMENT_TARGET=11.0 cargo build --release --target "$T" -p jarvis-cli -p jarvis-gui
done
UNI="target/universal"
mkdir -p "$UNI"
for B in jarvis jarvis-gui; do
  lipo -create -output "$UNI/$B" "target/aarch64-apple-darwin/release/$B" "target/x86_64-apple-darwin/release/$B"
done

APP="target/Jarvis Core.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
sed "s/VERSION/$VERSION/g" packaging/macos/Info.plist > "$APP/Contents/Info.plist"
cp "$UNI/jarvis-gui" "$UNI/jarvis" "$APP/Contents/MacOS/"
ICONSET="target/jarvis-core.iconset"
rm -rf "$ICONSET" && mkdir -p "$ICONSET"
for S in 16 32 128 256 512; do
  sips -z $S $S packaging/icons/jarvis-core-1024.png --out "$ICONSET/icon_${S}x${S}.png" >/dev/null
  D=$((S * 2))
  sips -z $D $D packaging/icons/jarvis-core-1024.png --out "$ICONSET/icon_${S}x${S}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/jarvis-core.icns"
# Assinatura ad hoc (sem conta de desenvolvedor): permite executar após remover a quarentena.
codesign --force --deep --sign - "$APP"

STAGE="target/dmg"
rm -rf "$STAGE" && mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
cp README.md LICENSE "$STAGE/"
DMG="dist/Jarvis_Core-$VERSION-macos-universal.dmg"
rm -f "$DMG"
hdiutil create -volname "Jarvis Core" -srcfolder "$STAGE" -ov -format UDZO "$DMG"

CLI="target/jarvis-cli-macos"
rm -rf "$CLI" && mkdir -p "$CLI"
cp "$UNI/jarvis" "$UNI/jarvis-gui" README.md LICENSE "$CLI/"
tar -C target -czf "dist/Jarvis_Core-$VERSION-macos-universal.tar.gz" jarvis-cli-macos
echo "Gerados: $DMG e dist/Jarvis_Core-$VERSION-macos-universal.tar.gz"
