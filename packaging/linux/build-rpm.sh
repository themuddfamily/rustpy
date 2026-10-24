#!/usr/bin/env bash
# Builds an RPM package (Fedora, RHEL, openSUSE) with cargo-generate-rpm
# (https://github.com/cat-in-136/cargo-generate-rpm) from the [package.metadata.generate-rpm]
# section of crates/rustpy/Cargo.toml:
#   /usr/bin/rustpy
#   /usr/share/applications/rustpy.desktop
#   /usr/share/icons/hicolor/scalable/apps/rustpy.svg
#   /usr/share/metainfo/dev.rustpy.RustPy.metainfo.xml
#   /usr/share/mime/packages/rustpy.xml
# Requires: libxkbcommon.so.0, libodbc.so.2 and other linked libraries.
# Source builds need unixODBC-devel; the runtime must provide libodbc.so.2.
# Recommends: python3, libxkbcommon-x11, libEGL or libGL.
#
# Usage: packaging/linux/build-rpm.sh [--no-build] [--no-strip] [--target TRIPLE] [--out DIR]
#   --no-build        package the existing release binary instead of running
#                     `cargo build --locked --release -p rustpy --features "${RUSTPY_RELEASE_FEATURES-native-web-preview}"` first
#   --no-strip        keep the debug info (the release profile has debug = 1)
#   --target TRIPLE   package target/TRIPLE/release/rustpy, e.g. aarch64-unknown-linux-gnu
#   --out DIR         output directory (default: <target dir>/generate-rpm)
#
# The binary is copied to <target dir>/rpm-staging and stripped there, so
# target/release/rustpy keeps its debug info. A pre-release version such as 0.3.0-eap.1
# becomes the RPM version 0.3.0~eap.1, which sorts before 0.3.0.
#
# Requires cargo-generate-rpm (installed with `cargo install cargo-generate-rpm --locked`
# when missing); rpm is not needed. Output: DIR/rustpy-<version>-1.<arch>.rpm (printed at
# the end). CARGO_TARGET_DIR is honoured.
set -euo pipefail

usage() { sed -n '2,/^set -euo pipefail$/{/^set /d;s/^# \{0,1\}//;p;}' "$0"; }
die() { echo "build-rpm: $*" >&2; exit 1; }

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
    *) echo "build-rpm: unknown option: $1 (see --help)" >&2; exit 2 ;;
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
case "$TARGET_DIR" in
  /*) ;;
  *) TARGET_DIR="$ROOT/$TARGET_DIR" ;;
esac
OUT="${OUT:-$TARGET_DIR/generate-rpm}"
VERSION="$(awk -F'"' '/^\[/ { section = $0 } section == "[workspace.package]" && $1 ~ /^version *= *$/ { print $2; exit }' Cargo.toml)"
[[ -n $VERSION ]] || die "cannot read the [workspace.package] version from Cargo.toml"

if ! cargo generate-rpm --version >/dev/null 2>&1; then
  echo "build-rpm: cargo-generate-rpm not found; installing it with 'cargo install cargo-generate-rpm --locked'" >&2
  cargo install cargo-generate-rpm --locked
fi

if ((BUILD)); then
  if [[ -n $TARGET ]]; then
    cargo build --locked --release -p rustpy --features "${RUSTPY_RELEASE_FEATURES-native-web-preview}" --target "$TARGET"
  else
    cargo build --locked --release -p rustpy --features "${RUSTPY_RELEASE_FEATURES-native-web-preview}"
  fi
fi
if [[ -n $TARGET ]]; then
  BIN="$TARGET_DIR/$TARGET/release/rustpy"
else
  BIN="$TARGET_DIR/release/rustpy"
fi
[[ -x $BIN ]] || die "release binary not found: $BIN (run without --no-build)"

# cargo-generate-rpm reads `target/release/rustpy` from --target-dir: point it at a staging
# directory with a stripped copy.
STAGE="$TARGET_DIR/rpm-staging"
STAGE_BIN_DIR="$STAGE/${TARGET:+$TARGET/}release"
rm -rf -- "$STAGE"
install -d "$STAGE_BIN_DIR"
install -m 755 "$BIN" "$STAGE_BIN_DIR/rustpy"
if ((STRIP)); then
  STRIP_TOOL="strip"
  case "$TARGET" in
    aarch64-*) [[ $(uname -m) == aarch64 ]] || STRIP_TOOL=aarch64-linux-gnu-strip ;;
    x86_64-*) [[ $(uname -m) == x86_64 ]] || STRIP_TOOL=x86_64-linux-gnu-strip ;;
  esac
  if command -v "$STRIP_TOOL" >/dev/null 2>&1; then
    "$STRIP_TOOL" --strip-debug "$STAGE_BIN_DIR/rustpy"
  else
    echo "build-rpm: note: $STRIP_TOOL not found; packaging the binary with its debug info" >&2
  fi
fi

mkdir -p packaging/generated/runtime-empty
python3 packaging/stage-native-licenses.py --binary "$STAGE_BIN_DIR/rustpy" --runtime-root packaging/generated/runtime-empty --output packaging/generated/Native-Licenses.zip

ARGS=(-p crates/rustpy --target-dir "$STAGE" -o "$OUT")
if [[ -n $TARGET ]]; then
  ARGS+=(--target "$TARGET")
  ARCH="${TARGET%%-*}"
else
  ARCH="$(uname -m)"
fi
# RPM versions cannot contain "-"; "~" sorts before the final release.
RPM_VERSION="${VERSION//-/\~}"
if [[ $RPM_VERSION != "$VERSION" ]]; then
  printf 'version = "%s"\n' "$RPM_VERSION" > "$STAGE/version.toml"
  ARGS+=("--metadata-overwrite=$STAGE/version.toml")
fi
mkdir -p -- "$OUT"
RPM="$OUT/rustpy-$RPM_VERSION-1.$ARCH.rpm"
rm -f -- "$RPM"
cargo generate-rpm "${ARGS[@]}"
[[ -f $RPM ]] || die "cargo generate-rpm did not write $RPM"
echo "RPM package: $RPM"
if command -v rpm >/dev/null 2>&1; then
  rpm -qp --queryformat '%{NAME} %{VERSION}-%{RELEASE} %{ARCH}\n' "$RPM"
  rpm -qpR "$RPM" | sed 's/^/  requires: /'
  rpm -qpl "$RPM" | sed 's/^/  /'
fi
