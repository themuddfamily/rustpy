#!/usr/bin/env bash
# Cross-builds and assembles Windows acceptance pack 2 from Linux/WSL with
# cargo-xwin (never runs a Windows executable). Usage:
#   qa/windows/assemble-pack-2.sh <output dir>
# Honours CARGO_TARGET_DIR. Builds rustpy.exe exactly as release.yml does
# (--release --features native-web-preview) plus the two acceptance helpers and
# the Chromium launch diagnostic.
set -euo pipefail
cd "$(dirname "$0")/../.."
out="${1:?usage: assemble-pack-2.sh <output dir>}"
target_dir="${CARGO_TARGET_DIR:-$PWD/target}"
triple=x86_64-pc-windows-msvc
jobs="${CARGO_BUILD_JOBS:-2}"
cargo xwin build --release -j"$jobs" --target "$triple" -p rustpy --features native-web-preview
cargo xwin build --release -j"$jobs" --target "$triple" -p rustpy-ui --features native-web-preview \
    --example windows_browser_acceptance --example windows_chromium_diagnostic
cargo xwin build --release -j"$jobs" --target "$triple" -p rustpy-project \
    --example windows_interpreter_acceptance
bin="$target_dir/$triple/release"
mkdir -p "$out"
cp "$bin/rustpy.exe" "$out/"
# The PDB (~400 MB) stays in the target dir unless asked for.
if [ -n "${INCLUDE_PDB:-}" ]; then cp "$bin/rustpy.pdb" "$out/"; fi
cp "$bin/examples/windows_browser_acceptance.exe" "$bin/examples/windows_interpreter_acceptance.exe" \
    "$bin/examples/windows_chromium_diagnostic.exe" "$out/"
cp qa/windows/run-windows-acceptance-2.ps1 qa/windows/run-chromium-diagnostic.ps1 qa/windows/README.md "$out/"
cp packaging/windows/rustpy.wxs THIRD-PARTY-NOTICES.txt "$out/"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)"
{
    echo "version: $version"
    echo "commit: $(git rev-parse --short HEAD)$(git diff --quiet HEAD -- crates qa packaging || echo '+dirty')"
    echo "built: $(date -u +%Y-%m-%dT%H:%M:%SZ) ($triple, release, native-web-preview)"
} > "$out/pack-info.txt"
(cd "$out" && sha256sum -- *.exe *.ps1 *.wxs > SHA256SUMS)
ls -la "$out"
