#!/usr/bin/env bash
# Cross-builds the Windows arm64 rustpy.exe from Linux/WSL with cargo-xwin.
# Usage: packaging/windows/cross-build-arm64.sh [extra cargo args]
# Output: $CARGO_TARGET_DIR/aarch64-pc-windows-msvc/release/rustpy.exe
#
# Two C dependencies need help when cross-compiling (native MSVC builds in
# release.yml need neither):
# - ring calls plain `clang` for aarch64 but cargo-xwin's flags are clang-cl
#   style (`/imsvc <dir>`); a shim rewrites them to `-isystem <dir>`.
# - libgit2 uses _InterlockedAdd64, which MSVC provides as an ARM64 intrinsic
#   but clang's headers do not declare; it is defined from
#   _InterlockedExchangeAdd64.
set -euo pipefail
cd "$(dirname "$0")/../.."
triple=aarch64-pc-windows-msvc
# The shim lives in the target dir, not a temp dir: cargo-xwin caches a
# clang-cl symlink to whichever clang is first on PATH, so it must stay valid.
shim="$(realpath -m "${CARGO_TARGET_DIR:-target}")/xwin-arm64-clang-shim"
mkdir -p "$shim"
real_clang="$(command -v clang)"
cat > "$shim/clang" <<SHIM
#!/usr/bin/env bash
case "\$(basename "\$0")" in *clang-cl*) exec -a clang-cl "$real_clang" "\$@" ;; esac
args=(); for a in "\$@"; do [ "\$a" = "/imsvc" ] && a="-isystem"; args+=("\$a"); done
exec "$real_clang" "\${args[@]}"
SHIM
chmod +x "$shim/clang"
export PATH="$shim:$PATH"
# cc-rs reads the hyphenated CFLAGS_<triple> first; bash cannot export that name.
env "CFLAGS_${triple}=-D_InterlockedAdd64(a,b)=(_InterlockedExchangeAdd64((a),(b))+(b))" \
    cargo xwin build --release -j"${CARGO_BUILD_JOBS:-2}" --target "$triple" -p rustpy --features native-web-preview "$@"
file "${CARGO_TARGET_DIR:-target}/$triple/release/rustpy.exe" 2>/dev/null || true
