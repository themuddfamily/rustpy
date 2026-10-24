#!/usr/bin/env bash
# Builds rustpy_<version>_<arch>.snap with snapcraft from packaging/linux/snap/snapcraft.yaml
# (classic confinement, base core24).
#
# Usage: packaging/linux/build-snap.sh [--out DIR] [snapcraft options...]
#   --out DIR   output directory (default: <target dir>/snap)
#   e.g. --destructive-mode to build on this machine (an Ubuntu 24.04 host) instead of in an
#   LXD or Multipass instance, or --use-lxd.
#
# snapcraft expects snap/snapcraft.yaml in the project root, so the script stages a copy of
# the repository (without target/ and .git) in <target dir>/snap-staging with the file
# there, and runs snapcraft in it. Install the result with
#   sudo snap install --classic --dangerous <target dir>/snap/rustpy_<version>_<arch>.snap
#
# Requires snapcraft (`sudo snap install snapcraft --classic`). CARGO_TARGET_DIR is honoured.
set -euo pipefail

usage() { sed -n '2,/^set -euo pipefail$/{/^set /d;s/^# \{0,1\}//;p;}' "$0"; }
die() { echo "build-snap: $*" >&2; exit 1; }

OUT=""
ARGS=()
while (($#)); do
  case "$1" in
    --out) (($# >= 2)) || die "--out needs a directory"; OUT="$2"; shift ;;
    --out=*) OUT="${1#--out=}" ;;
    -h|--help) usage; exit 0 ;;
    *) ARGS+=("$1") ;;
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
command -v snapcraft >/dev/null 2>&1 || die "snapcraft not found (sudo snap install snapcraft --classic)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
case "$TARGET_DIR" in
  /*) ;;
  *) TARGET_DIR="$ROOT/$TARGET_DIR" ;;
esac
OUT="${OUT:-$TARGET_DIR/snap}"
STAGE="$TARGET_DIR/snap-staging"

rm -rf -- "$STAGE"
mkdir -p -- "$STAGE/snap" "$OUT"
# The sources, without build output (which also holds the staging directory), VCS data or
# earlier packages.
EXCLUDES=(--exclude=./target --exclude=./.git --exclude=./.claude --exclude=./.flatpak-builder --exclude='*.snap')
if [[ $TARGET_DIR == "$ROOT"/* ]]; then
  EXCLUDES+=("--exclude=./${TARGET_DIR#"$ROOT"/}")
fi
tar -C "$ROOT" "${EXCLUDES[@]}" -cf - . | tar -C "$STAGE" -xf -
cp packaging/linux/snap/snapcraft.yaml "$STAGE/snap/snapcraft.yaml"

(cd "$STAGE" && snapcraft ${ARGS[@]+"${ARGS[@]}"})
found=0
for snap in "$STAGE"/*.snap; do
  [[ -f $snap ]] || continue
  mv -f -- "$snap" "$OUT/"
  echo "Snap: $OUT/$(basename -- "$snap")"
  found=1
done
((found)) || die "snapcraft did not produce a .snap in $STAGE"
