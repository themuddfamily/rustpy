#!/usr/bin/env bash
# Cross-builds rustpy.exe and assembles the MSIX acceptance pack from Linux/WSL
# (never runs a Windows executable). Usage:
#   qa/windows/assemble-msix-pack.sh <output dir> [x64|arm64]
# Honours CARGO_TARGET_DIR and MAKEMSIX / MSIX_TOOLS_DIR (see
# packaging/windows/build-msix.sh; pass BUILD_MAKEMSIX=1 to build makemsix when
# it is missing). NO_BUILD=1 packages the existing release rustpy.exe.
set -euo pipefail
cd "$(dirname "$0")/../.."
out="${1:?usage: assemble-msix-pack.sh <output dir> [x64|arm64]}"
arch="${2:-x64}"
target_dir="${CARGO_TARGET_DIR:-$PWD/target}"
jobs="${CARGO_BUILD_JOBS:-2}"
if [ -z "${NO_BUILD:-}" ]; then
    case "$arch" in
        x64) cargo xwin build --release -j"$jobs" --target x86_64-pc-windows-msvc -p rustpy --features native-web-preview ;;
        arm64) packaging/windows/cross-build-arm64.sh ;;
        *) echo "unsupported arch $arch" >&2; exit 2 ;;
    esac
fi
build_out="$target_dir/msix"
packaging/windows/build-msix.sh --arch "$arch" --sign --out "$build_out" ${BUILD_MAKEMSIX:+--build-makemsix}
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"
mkdir -p "$out"
cp "$build_out/rustpy-$version-windows-$arch-test-signed.msix" "$build_out/rustpy-msix-test.cer" "$out/"
cp qa/windows/run-msix-acceptance.ps1 qa/windows/README.md "$out/"
{
    echo "version: $version"
    echo "commit: $(git rev-parse --short HEAD)$(git diff --quiet HEAD -- crates qa packaging || echo '+dirty')"
    echo "built: $(date -u +%Y-%m-%dT%H:%M:%SZ) ($arch, release, native-web-preview, test-signed MSIX)"
} > "$out/pack-info.txt"
(cd "$out" && sha256sum -- *.msix *.cer *.ps1 > SHA256SUMS)
ls -la "$out"
