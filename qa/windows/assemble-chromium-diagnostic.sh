#!/usr/bin/env bash
# Cross-builds and assembles the small Windows Chromium launch diagnostic pack
# from Linux/WSL with cargo-xwin (never runs a Windows executable). Usage:
#   qa/windows/assemble-chromium-diagnostic.sh <output dir>
# Honours CARGO_TARGET_DIR and CARGO_BUILD_JOBS.
set -euo pipefail
cd "$(dirname "$0")/../.."
out="${1:?usage: assemble-chromium-diagnostic.sh <output dir>}"
target_dir="${CARGO_TARGET_DIR:-$PWD/target}"
triple=x86_64-pc-windows-msvc
jobs="${CARGO_BUILD_JOBS:-2}"
cargo xwin build --release -j"$jobs" --target "$triple" -p rustpy-ui --features native-web-preview \
    --example windows_chromium_diagnostic
bin="$target_dir/$triple/release"
mkdir -p "$out"
cp "$bin/examples/windows_chromium_diagnostic.exe" "$out/"
cp qa/windows/run-chromium-diagnostic.ps1 "$out/"
{
    echo "commit: $(git rev-parse --short HEAD)$(git diff --quiet HEAD -- crates qa || echo '+dirty')"
    echo "built: $(date -u +%Y-%m-%dT%H:%M:%SZ) ($triple, release)"
} > "$out/pack-info.txt"
(cd "$out" && sha256sum -- *.exe *.ps1 > SHA256SUMS)
ls -la "$out"
