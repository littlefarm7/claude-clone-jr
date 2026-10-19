#!/usr/bin/env bash
# Gera dist/Jarvis_Core-<versão>-x86_64.AppImage.
#
# Requisitos: Rust; appimagetool (baixado automaticamente se APPIMAGETOOL não
# estiver definido). Recomendado: cargo-zigbuild (pip install ziglang
# cargo-zigbuild) para gerar binários que exigem apenas glibc 2.31.
#
# Variáveis opcionais:
#   APPIMAGETOOL=/caminho/appimagetool
#   LLAMA_SERVER_DIR=/caminho/llama.cpp/build/bin  (inclui llama-server e suas .so)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
APPDIR="$ROOT/target/AppDir"
mkdir -p dist

if command -v cargo-zigbuild >/dev/null 2>&1; then
  TARGET=x86_64-unknown-linux-gnu
  cargo zigbuild --release --target "$TARGET.2.31" -p jarvis-cli -p jarvis-gui
  BIN="target/$TARGET/release"
else
  echo "aviso: cargo-zigbuild ausente; o binário exigirá a glibc desta máquina ($(ldd --version | head -1))" >&2
  cargo build --release -p jarvis-cli -p jarvis-gui
  BIN="target/release"
fi

rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/applications" "$APPDIR/usr/share/icons/hicolor/scalable/apps" "$APPDIR/usr/share/metainfo"
install -m755 "$BIN/jarvis" "$BIN/jarvis-gui" "$APPDIR/usr/bin/"
install -m755 packaging/appimage/AppRun "$APPDIR/AppRun"
install -m644 packaging/appimage/jarvis-core.desktop "$APPDIR/jarvis-core.desktop"
install -m644 packaging/appimage/jarvis-core.desktop "$APPDIR/usr/share/applications/"
install -m644 packaging/appimage/jarvis-core.svg "$APPDIR/jarvis-core.svg"
install -m644 packaging/appimage/jarvis-core.svg "$APPDIR/usr/share/icons/hicolor/scalable/apps/"
install -m644 packaging/appimage/io.github.littlefarm7.jarvis_core.appdata.xml "$APPDIR/usr/share/metainfo/"
ln -sf jarvis-core.svg "$APPDIR/.DirIcon"

if [ -n "${LLAMA_SERVER_DIR:-}" ]; then
  mkdir -p "$APPDIR/usr/lib"
  install -m755 "$LLAMA_SERVER_DIR/llama-server" "$APPDIR/usr/bin/"
  find "$LLAMA_SERVER_DIR" -maxdepth 1 -name '*.so*' -exec cp -a {} "$APPDIR/usr/lib/" \;
  echo "llama-server incluído de $LLAMA_SERVER_DIR"
fi

TOOL="${APPIMAGETOOL:-}"
if [ -z "$TOOL" ]; then
  TOOL="$ROOT/target/appimagetool"
  if [ ! -x "$TOOL" ]; then
    curl -fsSL -o "$TOOL" https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
    chmod +x "$TOOL"
  fi
fi
OUT="dist/Jarvis_Core-$VERSION-x86_64.AppImage"
# APPIMAGE_EXTRACT_AND_RUN permite rodar o appimagetool sem FUSE (containers/CI).
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$TOOL" --no-appstream "$APPDIR" "$OUT"
echo "AppImage: $OUT"
