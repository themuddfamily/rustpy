#!/usr/bin/env bash
# Builds a Debian/Ubuntu package with cargo-deb (https://github.com/kornelski/cargo-deb)
# from the [package.metadata.deb] section of crates/rustpy/Cargo.toml:
#   /usr/bin/rustpy
#   /usr/share/applications/rustpy.desktop
#   /usr/share/icons/hicolor/scalable/apps/rustpy.svg
# Depends: libgtk-3-0 | libxkbcommon0 and libodbc2, plus dpkg-shlibdeps ($auto).
#
# Usage: packaging/linux/build-deb.sh [cargo-deb options...]
#   e.g. --no-build (package the existing target/release/rustpy),
#        --target aarch64-unknown-linux-gnu, --deb-revision 2, -o DIR
#   (`cargo deb --help` lists metadata options). Builds/stages/strips the release
#   binary explicitly, then invokes cargo-deb --no-build --no-strip so the linkage
#   manifest covers the bytes installed in the package. Debug-splitting/custom
#   profile options are rejected because they would change the reviewed payload.
#
# Requires cargo-deb (installed with `cargo install cargo-deb --locked` when missing)
# and dpkg-dev, whose dpkg-shlibdeps computes the $auto library dependencies.
# Source builds also need unixodbc-dev; libodbc2 supplies the installed runtime.
# Output: target/debian/rustpy_<version>-1_<arch>.deb (the path is printed at the end).
set -euo pipefail

usage() { sed -n '2,/^set -euo pipefail$/{/^set /d;s/^# \{0,1\}//;p;}' "$0"; }

# Stage and strip ourselves so the linkage manifest binds the exact packaged bytes.
ARGS=(-p rustpy --features "${RUSTPY_RELEASE_FEATURES-native-web-preview}" --locked --no-build --no-strip)
FORWARD=()
BUILD_ARGS=(--locked --release -p rustpy --features "${RUSTPY_RELEASE_FEATURES-native-web-preview}")
BUILD=1
STRIP=1
TARGET=""
while (($#)); do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --no-build) BUILD=0 ;;
    --strip) STRIP=1 ;;
    --no-strip) STRIP=0 ;;
    --locked) ;;
    --target)
      (($# >= 2)) || { echo 'build-deb: --target needs a triple' >&2; exit 2; }
      TARGET="$2"; shift ;;
    --target=*) TARGET="${1#--target=}" ;;
    --features)
      (($# >= 2)) || { echo 'build-deb: --features needs a value' >&2; exit 2; }
      BUILD_ARGS+=(--features "$2"); FORWARD+=(--features "$2"); shift ;;
    --features=*) BUILD_ARGS+=("$1"); FORWARD+=("$1") ;;
    --all-features|--no-default-features) BUILD_ARGS+=("$1"); FORWARD+=("$1") ;;
    --separate-debug-symbols|--dbgsym|--compress-debug-symbols|--profile|--profile=*|--target-dir|--target-dir=*)
      echo "build-deb: $1 can transform or relocate the binary after linkage review; use the release staging recipe with --no-strip or --no-build instead" >&2; exit 2 ;;
    *) FORWARD+=("$1") ;;
  esac
  shift
done
if [[ -n $TARGET ]]; then BUILD_ARGS+=(--target "$TARGET"); ARGS+=(--target "$TARGET"); fi

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
python3 packaging/update-notices.py --check

if ! cargo deb --version >/dev/null 2>&1; then
  echo "build-deb: cargo-deb not found; installing it with 'cargo install cargo-deb --locked'" >&2
  cargo install cargo-deb --locked
fi
if ! command -v dpkg-shlibdeps >/dev/null 2>&1; then
  echo "build-deb: warning: dpkg-shlibdeps (package dpkg-dev) is missing; the \$auto library dependencies will be left out" >&2
fi

TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
[[ $TARGET_DIR == /* ]] || TARGET_DIR="$ROOT/$TARGET_DIR"
if ((BUILD)); then cargo build "${BUILD_ARGS[@]}"; fi
BIN="$TARGET_DIR/${TARGET:+$TARGET/}release/rustpy"
[[ -x $BIN ]] || { echo "build-deb: missing release binary $BIN" >&2; exit 1; }
STAGE="$TARGET_DIR/deb-staging"
rm -rf -- "$STAGE"
STAGE_BIN_DIR="$STAGE/${TARGET:+$TARGET/}release"
install -Dm755 "$BIN" "$STAGE_BIN_DIR/rustpy"
if ((STRIP)); then
  STRIP_TOOL=strip
  case "$TARGET" in
    aarch64-*) [[ $(uname -m) == aarch64 ]] || STRIP_TOOL=aarch64-linux-gnu-strip ;;
    x86_64-*) [[ $(uname -m) == x86_64 ]] || STRIP_TOOL=x86_64-linux-gnu-strip ;;
  esac
  "$STRIP_TOOL" --strip-unneeded "$STAGE_BIN_DIR/rustpy"
fi
mkdir -p packaging/generated/runtime-empty
python3 packaging/stage-native-licenses.py --binary "$STAGE_BIN_DIR/rustpy" --runtime-root packaging/generated/runtime-empty --output packaging/generated/Native-Licenses.zip
# CARGO_TARGET_DIR redirects cargo-deb's target/release asset into the reviewed stage.
DEB="$(CARGO_TARGET_DIR="$STAGE" cargo deb "${ARGS[@]}" "${FORWARD[@]}" | tail -n 1)"
if [[ ! -f $DEB ]]; then
  echo "build-deb: cargo-deb did not report a package path (got: '$DEB')" >&2
  exit 1
fi
echo "Debian package: $DEB"
# A cross build (e.g. --target aarch64-unknown-linux-gnu on an x86_64 host) without the
# target architecture's libraries installed (dpkg --add-architecture arm64 plus the
# libgtk-3-dev:arm64 ... packages) leaves dpkg-shlibdeps unable to resolve $auto. cargo-deb
# then writes a package without libc6, WebKitGTK or OpenSSL dependencies, which installs
# but cannot start. Refuse it unless RUSTPY_DEB_ALLOW_INCOMPLETE_DEPENDS=1.
if command -v dpkg-deb >/dev/null 2>&1 && [[ ${RUSTPY_DEB_ALLOW_INCOMPLETE_DEPENDS:-0} != 1 ]]; then
  if ! dpkg-deb --field "$DEB" Depends | grep -q 'libc6'; then
    echo "build-deb: $DEB has no automatic (dpkg-shlibdeps) library dependencies:" >&2
    echo "  Depends: $(dpkg-deb --field "$DEB" Depends)" >&2
    echo "build-deb: install the target architecture's libraries or build on a native host" >&2
    echo "build-deb: (RUSTPY_DEB_ALLOW_INCOMPLETE_DEPENDS=1 keeps the package anyway)" >&2
    rm -f -- "$DEB"
    exit 1
  fi
fi
if command -v dpkg-deb >/dev/null 2>&1; then
  dpkg-deb --field "$DEB" Package Version Architecture Depends Recommends
  dpkg-deb --contents "$DEB" | awk '$NF !~ /\/$/ { print "  " $NF }'
fi
