#!/usr/bin/env bash
# Builds the RustPy Flatpak (dev.rustpy.RustPy) with flatpak-builder and installs it for the
# current user.
#
# Usage: packaging/linux/flatpak/build-flatpak.sh [--sources-only] [--no-install] [--bundle]
#                                                 [flatpak-builder options...]
#   --sources-only  only (re)generate packaging/linux/flatpak/cargo-sources.json
#   --no-install    build into target/flatpak/repo without installing
#   --bundle        also write target/flatpak/RustPy-<version>-<arch>.flatpak, a single-file
#                   bundle for `flatpak install --user RustPy-....flatpak`
#
# Steps:
#   1. cargo-sources.json: flatpak-cargo-generator.py from flatpak-builder-tools (downloaded
#      once into target/flatpak/, or $FLATPAK_CARGO_GENERATOR) turns Cargo.lock into the list
#      of crate archives with their checksums. It needs Python 3 with aiohttp, tomlkit and
#      PyYAML: `uv` or `pipx` provide them automatically, otherwise
#      `pip install aiohttp tomlkit pyyaml`.
#   2. flatpak-builder downloads all sources, then builds offline in the sandbox with
#      org.freedesktop.Sdk and its rust-stable extension (installed from Flathub when
#      missing; the flathub remote is added for the user if needed).
#
# Environment:
#   CARGO_TARGET_DIR                 work directory parent (default: target)
#   FLATPAK_CARGO_GENERATOR          flatpak-cargo-generator.py to use (skips the download)
#   FLATPAK_CARGO_GENERATOR_URL      download URL (default: flatpak-builder-tools master)
set -euo pipefail

usage() { sed -n '2,/^set -euo pipefail$/{/^set /d;s/^# \{0,1\}//;p;}' "$0"; }
die() { echo "build-flatpak: $*" >&2; exit 1; }

SOURCES_ONLY=0
INSTALL=1
BUNDLE=0
ARCH=""
EXTRA=()
while (($#)); do
  case "$1" in
    --sources-only) SOURCES_ONLY=1 ;;
    --no-install) INSTALL=0 ;;
    --bundle) BUNDLE=1 ;;
    --arch) (($# >= 2)) || die "--arch needs an architecture"; ARCH="$2"; [[ -n $ARCH ]] || die "--arch needs an architecture"; shift ;;
    --arch=*) ARCH="${1#--arch=}"; [[ -n $ARCH ]] || die "--arch needs an architecture" ;;
    -h|--help) usage; exit 0 ;;
    *) EXTRA+=("$1") ;;
  esac
  shift
done

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$ROOT"
python3 packaging/update-notices.py --check
APP_ID=dev.rustpy.RustPy
MANIFEST="packaging/linux/flatpak/$APP_ID.yml"
SOURCES="packaging/linux/flatpak/cargo-sources.json"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
WORK="$TARGET_DIR/flatpak"
mkdir -p -- "$WORK"
VERSION="$(awk -F'"' '/^\[/ { section = $0 } section == "[workspace.package]" && $1 ~ /^version *= *$/ { print $2; exit }' Cargo.toml)"
[[ -n $VERSION ]] || die "cannot read the [workspace.package] version from Cargo.toml"

# --- 1. cargo-sources.json -----------------------------------------------------
GENERATOR="${FLATPAK_CARGO_GENERATOR:-}"
if [[ -z $GENERATOR ]]; then
  GENERATOR="$WORK/flatpak-cargo-generator.py"
  if [[ ! -f $GENERATOR ]]; then
    URL="${FLATPAK_CARGO_GENERATOR_URL:-https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py}"
    echo "Downloading flatpak-cargo-generator.py from $URL" >&2
    if command -v curl >/dev/null 2>&1; then
      curl -fsSL --retry 3 -o "$GENERATOR.part" "$URL"
    elif command -v wget >/dev/null 2>&1; then
      wget -qO "$GENERATOR.part" "$URL"
    else
      die "curl or wget is needed to download flatpak-cargo-generator.py (or set FLATPAK_CARGO_GENERATOR)"
    fi
    mv -f -- "$GENERATOR.part" "$GENERATOR"
  fi
fi
[[ -f $GENERATOR ]] || die "flatpak-cargo-generator.py not found: $GENERATOR"
# The script declares its dependencies inline (PEP 723): uv and pipx install them.
if command -v uv >/dev/null 2>&1; then
  uv run --quiet --script "$GENERATOR" Cargo.lock -o "$SOURCES"
elif command -v pipx >/dev/null 2>&1; then
  pipx run "$GENERATOR" Cargo.lock -o "$SOURCES"
elif python3 -c 'import aiohttp, tomlkit' >/dev/null 2>&1; then
  python3 "$GENERATOR" Cargo.lock -o "$SOURCES"
else
  die "flatpak-cargo-generator.py needs aiohttp and tomlkit: install uv or pipx, or run 'pip install aiohttp tomlkit pyyaml'"
fi
echo "Cargo sources: $SOURCES"
((SOURCES_ONLY)) && exit 0

# --- 2. flatpak-builder ---------------------------------------------------------
command -v flatpak-builder >/dev/null 2>&1 || die "flatpak-builder not found (e.g. 'sudo apt install flatpak-builder' or 'sudo dnf install flatpak-builder')"
ARCH="${ARCH:-$(flatpak --default-arch)}"
[[ $ARCH =~ ^[A-Za-z0-9_]+$ ]] || die "invalid Flatpak architecture: $ARCH"
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
ARGS=(--arch="$ARCH" --user --force-clean --install-deps-from=flathub --state-dir="$WORK/state" --repo="$WORK/repo")
if ((INSTALL)); then
  ARGS+=(--install)
fi
flatpak-builder "${ARGS[@]}" ${EXTRA[@]+"${EXTRA[@]}"} "$WORK/build-dir" "$MANIFEST"
if ((BUNDLE)); then
  BUNDLE_FILE="$WORK/RustPy-$VERSION-$ARCH.flatpak"
  flatpak build-bundle --arch="$ARCH" "$WORK/repo" "$BUNDLE_FILE" "$APP_ID"
  echo "Flatpak bundle: $BUNDLE_FILE"
fi
if ((INSTALL)); then
  echo "Installed $APP_ID for this user: run it with 'flatpak run $APP_ID'."
fi
