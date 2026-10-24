#!/usr/bin/env bash
# Archive layout is the updater contract: rustpy is a top-level regular executable.
# Libraries are not bundled. The host must provide libodbc.so.2 (Debian/Ubuntu
# libodbc2) for the mandatory database backend; builds require unixodbc-dev.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
python3 packaging/update-notices.py --check
target="${1:-$(rustc -vV | sed -n 's/^host: //p')}"
case "$target" in x86_64-unknown-linux-gnu) arch=x86_64;; aarch64-unknown-linux-gnu) arch=aarch64;; *) echo 'unsupported Linux target' >&2; exit 2;; esac
version="$(awk -F'"' '/^\[/ {s=$0} s=="[workspace.package]" && /^version/ {print $2;exit}' Cargo.toml)"
base="${CARGO_TARGET_DIR:-$root/target}"
out="${RUSTPY_DIST_DIR:-$base/dist}"
mkdir -p "$out"
if [[ ${RUSTPY_NO_BUILD:-0} != 1 ]]; then
  cargo build --locked --release -p rustpy --target "$target" --features "${RUSTPY_RELEASE_FEATURES-native-web-preview}"
fi
stage="$(mktemp -d "$out/archive.XXXXXX")"
trap 'rm -rf "$stage"' EXIT
install -m755 "$base/$target/release/rustpy" "$stage/rustpy"
# The release profile keeps line-level debug info (debug = 1, ~850 MB); ship the binary
# without it, as the other packages do. RUSTPY_NO_STRIP=1 keeps it.
if [[ ${RUSTPY_NO_STRIP:-0} != 1 ]]; then
  strip_tool="strip"
  [[ $(uname -m) == "$arch" ]] || strip_tool="$arch-linux-gnu-strip"
  if command -v "$strip_tool" >/dev/null 2>&1; then
    "$strip_tool" --strip-debug "$stage/rustpy"
  else
    echo "build-tar: note: $strip_tool not found; the binary keeps its debug info" >&2
  fi
fi
install -m644 THIRD-PARTY-NOTICES.txt "$stage/THIRD-PARTY-NOTICES.txt"
install -m644 LICENSE "$stage/LICENSE"
install -m644 packaging/linux/rustpy.desktop packaging/linux/rustpy.svg packaging/linux/rustpy-mime.xml "$stage/"
python3 packaging/stage-native-licenses.py --binary "$stage/rustpy" --runtime-root "$stage" --output "$stage/Native-Licenses.zip"
tar -C "$stage" -czf "$out/rustpy-$version-linux-$arch.tar.gz" rustpy rustpy.desktop rustpy.svg rustpy-mime.xml LICENSE THIRD-PARTY-NOTICES.txt Native-Licenses.zip
