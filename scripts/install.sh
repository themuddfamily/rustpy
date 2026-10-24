#!/usr/bin/env bash
# Builds RustPy in release mode and installs it for the current user (no root needed):
#   <prefix>/bin/rustpy
#   <prefix>/share/applications/rustpy.desktop   (Exec/TryExec use the installed binary)
#   <prefix>/share/icons/hicolor/scalable/apps/rustpy.svg
#   <prefix>/share/doc/rustpy/THIRD-PARTY-NOTICES.txt
#
# Usage: scripts/install.sh [--prefix DIR] [--no-build] [--no-strip] [--uninstall]
#   --prefix DIR  install prefix (default: $PREFIX, otherwise ~/.local)
#   --no-build    install the existing release binary instead of running
#                 `cargo build --locked --release -p rustpy` first
#   --no-strip    keep the debug info (the release profile has debug = 1)
#   --uninstall   remove those installed files again (settings are kept)
#
# CARGO_TARGET_DIR is honoured. Settings, logs and local history live elsewhere;
# see docs/configuration-directories.md.
set -euo pipefail

usage() { sed -n '2,/^set -euo pipefail$/{/^set /d;s/^# \{0,1\}//;p;}' "$0"; }
die() { echo "install: $*" >&2; exit 1; }

PREFIX="${PREFIX:-$HOME/.local}"
BUILD=1
STRIP=1
UNINSTALL=0
while (($#)); do
  case "$1" in
    --prefix) (($# >= 2)) || die "--prefix needs a directory"; PREFIX="$2"; shift ;;
    --prefix=*) PREFIX="${1#--prefix=}" ;;
    --no-build) BUILD=0 ;;
    --no-strip) STRIP=0 ;;
    --uninstall) UNINSTALL=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "install: unknown option: $1 (see --help)" >&2; exit 2 ;;
  esac
  shift
done
[[ -n $PREFIX ]] || die "the prefix is empty"
case "$PREFIX" in
  /*) ;;
  *) PREFIX="$PWD/$PREFIX" ;;
esac

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BIN_DIR="$PREFIX/bin"
APP_DIR="$PREFIX/share/applications"
ICON_DIR="$PREFIX/share/icons/hicolor/scalable/apps"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"

refresh_caches() {
  if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q "$APP_DIR" 2>/dev/null || true
  fi
  # Refresh an existing icon cache so it lists the new icon; none is created.
  if [[ -f $PREFIX/share/icons/hicolor/icon-theme.cache ]] && command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t -f "$PREFIX/share/icons/hicolor" 2>/dev/null || true
  fi
}

# Encodes a path as one argument of a desktop entry Exec key (Desktop Entry spec,
# "The Exec key"): "%" becomes "%%"; anything beyond plain path characters is quoted
# with " ` $ \ backslash-escaped inside the quotes; then backslashes are doubled
# for the string-level escaping of the value.
desktop_exec_arg() {
  local s="${1//%/%%}"
  if [[ $s == *[^A-Za-z0-9_./+,:@%=-]* ]]; then
    s="${s//\\/\\\\}"
    s="${s//\"/\\\"}"
    s="${s//\`/\\\`}"
    s="${s//\$/\\\$}"
    s="\"$s\""
  fi
  printf '%s' "${s//\\/\\\\}"
}

if ((UNINSTALL)); then
  rm -f -- "$BIN_DIR/rustpy" "$APP_DIR/rustpy.desktop" "$ICON_DIR/rustpy.svg" "$PREFIX/share/doc/rustpy/THIRD-PARTY-NOTICES.txt" "$PREFIX/share/doc/rustpy/LICENSE" "$PREFIX/share/doc/rustpy/Native-Licenses.zip"
  refresh_caches
  echo "Removed RustPy from $PREFIX (settings in ~/.config/rustpy or \$RUSTPY_CONFIG_DIR are kept)."
  exit 0
fi

python3 packaging/update-notices.py --check

if ((BUILD)); then
  cargo build --locked --release -p rustpy
fi
BIN="$TARGET_DIR/release/rustpy"
[[ -x $BIN ]] || die "release binary not found: $BIN (run without --no-build)"

install -d "$BIN_DIR" "$APP_DIR" "$ICON_DIR"
install -m 755 "$BIN" "$BIN_DIR/rustpy"
install -Dm644 THIRD-PARTY-NOTICES.txt "$PREFIX/share/doc/rustpy/THIRD-PARTY-NOTICES.txt"
install -Dm644 LICENSE "$PREFIX/share/doc/rustpy/LICENSE"
if ((STRIP)) && command -v strip >/dev/null 2>&1; then
  strip --strip-debug "$BIN_DIR/rustpy"
fi
python3 packaging/stage-native-licenses.py --runtime-file "$BIN_DIR/rustpy" --output "$PREFIX/share/doc/rustpy/Native-Licenses.zip"
install -m 644 packaging/linux/rustpy.svg "$ICON_DIR/rustpy.svg"

# The desktop entry points at the installed binary, so launchers work even when
# <prefix>/bin is not on the PATH of the desktop session.
exec_arg="$(desktop_exec_arg "$BIN_DIR/rustpy")"
try_exec="${BIN_DIR//\\/\\\\}/rustpy"
tmp="$(mktemp "$APP_DIR/.rustpy.desktop.XXXXXX")"
while IFS= read -r line || [[ -n $line ]]; do
  case "$line" in
    Exec=rustpy*) printf 'Exec=%s%s\n' "$exec_arg" "${line#Exec=rustpy}" ;;
    TryExec=*) printf 'TryExec=%s\n' "$try_exec" ;;
    *) printf '%s\n' "$line" ;;
  esac
done < packaging/linux/rustpy.desktop > "$tmp"
chmod 644 "$tmp"
mv -f -- "$tmp" "$APP_DIR/rustpy.desktop"
refresh_caches

echo "Installed $BIN_DIR/rustpy"
echo "          $APP_DIR/rustpy.desktop"
echo "          $ICON_DIR/rustpy.svg"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "Note: $BIN_DIR is not on your PATH; add it (e.g. export PATH=\"$BIN_DIR:\$PATH\" in ~/.profile) to run 'rustpy' from a terminal." ;;
esac
