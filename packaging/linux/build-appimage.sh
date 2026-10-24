#!/usr/bin/env bash
# Builds RustPy-<version>-<arch>.AppImage from target/release/rustpy.
#
# Usage: packaging/linux/build-appimage.sh [--no-build] [--no-strip] [--target TRIPLE] [--out DIR]
#   --no-build        package the existing release binary instead of running
#                     `cargo build --locked --release -p rustpy --features "${RUSTPY_RELEASE_FEATURES-native-web-preview}"` first
#   --no-strip        keep the debug info (the release profile has debug = 1)
#   --target TRIPLE   package target/TRIPLE/release/rustpy (e.g. aarch64-unknown-linux-gnu)
#                     for that architecture; appimagetool still runs on the host
#   --out DIR         output directory (default: <target dir>/appimage)
#
# Steps: build -> assemble DIR/RustPy.AppDir (AppRun, rustpy.desktop, rustpy.svg,
# .DirIcon, usr/bin/rustpy, usr/share/applications, usr/share/icons, the AppStream
# metadata in usr/share/metainfo) -> find appimagetool ($APPIMAGETOOL, then PATH, then a
# copy downloaded once into <target dir>/appimagetool) -> DIR/RustPy-<version>-<arch>.AppImage for the host
# architecture or the --target one.
#
# Environment:
#   CARGO_TARGET_DIR  where cargo puts the binary (default: target)
#   APPIMAGETOOL      appimagetool executable to use (skips the lookup and download)
#   APPIMAGETOOL_URL  download URL (default: the "continuous" GitHub release)
#
# No libraries are bundled: the AppImage needs a glibc at least as new as the build
# machine's and the host's X11/Wayland, xkbcommon and EGL/GL libraries (loaded at run time).
# The mandatory database backend also needs the host unixODBC driver manager
# (libodbc.so.2; Debian/Ubuntu libodbc2), even without native-web-preview.
# Building requires unixodbc-dev, not just the installed runtime package.
# The default native-web-preview feature additionally requires GTK3 and
# WebKitGTK 4.1 on the host; set RUSTPY_RELEASE_FEATURES='' for a build without it.
set -euo pipefail

usage() { sed -n '2,/^set -euo pipefail$/{/^set /d;s/^# \{0,1\}//;p;}' "$0"; }
die() { echo "build-appimage: $*" >&2; exit 1; }

BUILD=1
STRIP=1
TARGET=""
OUT=""
while (($#)); do
  case "$1" in
    --no-build) BUILD=0 ;;
    --no-strip) STRIP=0 ;;
    --target) (($# >= 2)) || die "--target needs a target triple"; TARGET="$2"; shift ;;
    --target=*) TARGET="${1#--target=}" ;;
    --out) (($# >= 2)) || die "--out needs a directory"; OUT="$2"; shift ;;
    --out=*) OUT="${1#--out=}" ;;
    -h|--help) usage; exit 0 ;;
    *) echo "build-appimage: unknown option: $1 (see --help)" >&2; exit 2 ;;
  esac
  shift
done
if [[ -n $OUT ]]; then
  mkdir -p -- "$OUT"
  OUT="$(cd -- "$OUT" && pwd)"
fi

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
python3 packaging/update-notices.py --check

TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
OUT="${OUT:-$TARGET_DIR/appimage}"
VERSION="$(awk -F'"' '/^\[/ { section = $0 } section == "[workspace.package]" && $1 ~ /^version *= *$/ { print $2; exit }' Cargo.toml)"
[[ -n $VERSION ]] || die "cannot read the [workspace.package] version from Cargo.toml"
appimage_arch() { # $1: uname -m or the first part of a target triple
  case "$1" in
    x86_64|amd64) echo x86_64 ;;
    aarch64|arm64) echo aarch64 ;;
    armv7l|armv7|armhf) echo armhf ;;
    i386|i586|i686) echo i686 ;;
    *) die "unsupported architecture: $1" ;;
  esac
}
HOST_ARCH="$(appimage_arch "$(uname -m)")"
ARCH="$HOST_ARCH"
if [[ -n $TARGET ]]; then
  ARCH="$(appimage_arch "${TARGET%%-*}")"
fi

if ((BUILD)); then
  cargo build --locked --release -p rustpy --features "${RUSTPY_RELEASE_FEATURES-native-web-preview}" ${TARGET:+--target "$TARGET"}
fi
BIN="$TARGET_DIR/${TARGET:+$TARGET/}release/rustpy"
[[ -x $BIN ]] || die "release binary not found: $BIN (run without --no-build)"

# --- AppDir -----------------------------------------------------------------
APPDIR="$OUT/RustPy.AppDir"
rm -rf -- "$APPDIR"
install -d "$APPDIR/usr/bin" "$APPDIR/usr/share/applications" "$APPDIR/usr/share/icons/hicolor/scalable/apps" "$APPDIR/usr/share/metainfo"
install -m 755 "$BIN" "$APPDIR/usr/bin/rustpy"
install -Dm644 THIRD-PARTY-NOTICES.txt "$APPDIR/usr/share/doc/rustpy/THIRD-PARTY-NOTICES.txt"
install -Dm644 LICENSE "$APPDIR/usr/share/doc/rustpy/LICENSE"
STRIP_TOOL="strip"
if [[ $ARCH != "$HOST_ARCH" ]]; then
  STRIP_TOOL="${TARGET%%-*}-linux-gnu-strip"
fi
if ((STRIP)) && command -v "$STRIP_TOOL" >/dev/null 2>&1; then
  "$STRIP_TOOL" --strip-debug "$APPDIR/usr/bin/rustpy"
elif ((STRIP)); then
  echo "build-appimage: note: $STRIP_TOOL not found; the binary keeps its debug info" >&2
fi
install -m 644 packaging/linux/rustpy.desktop "$APPDIR/usr/share/applications/rustpy.desktop"
install -m 644 packaging/linux/rustpy.svg "$APPDIR/usr/share/icons/hicolor/scalable/apps/rustpy.svg"
# AppStream metadata for AppImage catalogs (its launchable is rustpy.desktop).
install -m 644 packaging/linux/dev.rustpy.RustPy.metainfo.xml "$APPDIR/usr/share/metainfo/dev.rustpy.RustPy.metainfo.xml"
# appimagetool wants the desktop file, and the icon named by its Icon= key, in the AppDir root.
install -m 644 packaging/linux/rustpy.desktop "$APPDIR/rustpy.desktop"
install -m 644 packaging/linux/rustpy.svg "$APPDIR/rustpy.svg"
ln -s rustpy.svg "$APPDIR/.DirIcon"
cat > "$APPDIR/AppRun" <<'APPRUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
export PATH="$HERE/usr/bin:$PATH"
exec "$HERE/usr/bin/rustpy" "$@"
APPRUN
chmod 755 "$APPDIR/AppRun"

# --- appimagetool -----------------------------------------------------------
TOOL="${APPIMAGETOOL:-}"
if [[ -z $TOOL ]] && command -v appimagetool >/dev/null 2>&1; then
  TOOL="$(command -v appimagetool)"
fi
if [[ -z $TOOL ]]; then
  # Cached outside the output directory so release checksums never pick it up as an asset.
  mkdir -p -- "$TARGET_DIR/appimagetool"
  TOOL="$TARGET_DIR/appimagetool/appimagetool-$HOST_ARCH.AppImage"
  if [[ ! -x $TOOL ]]; then
    URL="${APPIMAGETOOL_URL:-https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$HOST_ARCH.AppImage}"
    echo "Downloading appimagetool from $URL" >&2
    if command -v curl >/dev/null 2>&1; then
      curl -fL --retry 3 -o "$TOOL.part" "$URL"
    elif command -v wget >/dev/null 2>&1; then
      wget -O "$TOOL.part" "$URL"
    else
      die "curl or wget is needed to download appimagetool (or set APPIMAGETOOL)"
    fi
    chmod 755 "$TOOL.part"
    mv -f -- "$TOOL.part" "$TOOL"
  fi
fi
[[ -x $TOOL ]] || die "appimagetool is not executable: $TOOL"

# appimagetool otherwise downloads and injects an unreviewed runtime. Retain the
# exact supplied runtime inside AppDir so its source/license evidence is covered
# by the same manifest gate as the injected bytes.
[[ -n ${APPIMAGETOOL_RUNTIME_FILE:-} && -f $APPIMAGETOOL_RUNTIME_FILE ]] || die "set APPIMAGETOOL_RUNTIME_FILE to the reviewed AppImage runtime for $ARCH"
RUNTIME_REL="usr/lib/rustpy/appimage-runtime"
install -Dm755 "$APPIMAGETOOL_RUNTIME_FILE" "$APPDIR/$RUNTIME_REL"
python3 packaging/stage-native-licenses.py --binary "$APPDIR/usr/bin/rustpy" --runtime-root "$APPDIR" --required-native-file "$RUNTIME_REL" --output "$APPDIR/usr/share/doc/rustpy/Native-Licenses.zip"

# APPIMAGE_EXTRACT_AND_RUN lets an AppImage-packaged appimagetool run without FUSE
# (containers, CI). --runtime-file forbids implicit runtime selection/download.
OUTFILE="$OUT/RustPy-$VERSION-$ARCH.AppImage"
rm -f -- "$OUTFILE"
ARCH="$ARCH" APPIMAGE_EXTRACT_AND_RUN=1 "$TOOL" --runtime-file "$APPDIR/$RUNTIME_REL" --no-appstream "$APPDIR" "$OUTFILE"
echo "AppImage: $OUTFILE"
