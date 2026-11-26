#!/usr/bin/env bash
# Cross-compile and package RustPy using Linux tools only.
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/build-windows.sh [--fast [FAST OPTIONS]] [--out DIR]

Builds a native 64-bit Windows executable and portable ZIP from WSL/Linux.
No Windows installation, SDK, or Visual Studio is needed.

One-time setup on Ubuntu/Debian:
  sudo apt-get update
  sudo apt-get install -y mingw-w64 zip
  rustup target add x86_64-pc-windows-gnu
  sudo apt-get install -y lld        # optional, used by --fast

Default mode: optimized release build (thin LTO), the same settings as
`cargo build --release`. Output (under CARGO_TARGET_DIR, or target/ by default):
  windows-portable/rustpy.exe
  rustpy-windows-x86_64.zip
  rustpy-windows-x86_64-native-licenses.zip, the corresponding source and
    licence evidence, only when RUSTPY_NATIVE_RUNTIME_MANIFEST names that
    evidence for this executable; without it the build is for local use only

--fast: a build for the edit-build-test loop. It uses a separate `wintest`
profile, so it never overwrites the default output: no LTO, many codegen
units, no debug information, incremental compilation of workspace crates,
dependencies at opt-level 3, workspace crates at --opt-level (default 2), and
an LLD linker. Keep the same CARGO_TARGET_DIR between builds to get fast
rebuilds. Not for distribution: no native license bundle, installer or
signature. Output:
  windows-test/rustpy.exe
  rustpy-windows-x86_64-test.zip
  --toolchain T    msvc: the x86_64-pc-windows-msvc target of official
                   releases, built with cargo-xwin, clang-cl and lld-link,
                   with native-web-preview enabled. Needs cargo-xwin and its
                   Microsoft CRT/SDK cache, which this script never downloads.
                   gnu: MinGW (x86_64-pc-windows-gnu), linked with ld.lld when
                   installed. auto (default): msvc when available, else gnu.
  --opt-level N    optimization level of the workspace crates (0-3, default 2)
  --ui-opt-level N optimization level of the large rustpy-ui crates (0-3,
                   default 0): an edit there rebuilds in about 20 s at 0 and
                   about a minute at 1; raise it if the UI feels slow
  --linker NAME    gnu only: lld (default when ld.lld is installed) or bfd
  --features LIST  Cargo features of the rustpy package, replacing the
                   default (native-web-preview for msvc, none for gnu);
                   --features= builds without features
  --no-zip         stop after staging windows-test/; skip the ZIP

--out DIR: also copy the ZIP into DIR (for example a folder under /mnt/c).

Extract the ZIP and run rustpy.exe on Windows. Python is needed to run Python
projects. Settings are stored in the user configuration directory; this does
not bundle Python or make settings travel with the executable.
EOF
}

fast=0
opt_level=
ui_opt_level=
linker=
toolchain=
features=
features_set=0
out_dir=
make_zip=1
while (( $# )); do
    case $1 in
        -h|--help) usage; exit 0 ;;
        --fast) fast=1 ;;
        --opt-level) opt_level=${2:?--opt-level needs a value}; shift ;;
        --opt-level=*) opt_level=${1#*=} ;;
        --ui-opt-level) ui_opt_level=${2:?--ui-opt-level needs a value}; shift ;;
        --ui-opt-level=*) ui_opt_level=${1#*=} ;;
        --linker) linker=${2:?--linker needs a value}; shift ;;
        --linker=*) linker=${1#*=} ;;
        --toolchain) toolchain=${2:?--toolchain needs a value}; shift ;;
        --toolchain=*) toolchain=${1#*=} ;;
        --features) features=${2:?--features needs a value}; features_set=1; shift ;;
        --features=*) features=${1#*=}; features_set=1 ;;
        --no-zip) make_zip=0 ;;
        --out) out_dir=${2:?--out needs a directory}; shift ;;
        --out=*) out_dir=${1#*=} ;;
        *) echo 'Usage: scripts/build-windows.sh [--fast [FAST OPTIONS]] [--out DIR] [--help]' >&2
           exit 2 ;;
    esac
    shift
done
if (( !fast )) && { [[ -n $opt_level || -n $ui_opt_level || -n $linker || -n $toolchain ]] || (( features_set || !make_zip )); }; then
    echo 'Only --out applies without --fast; the other options need --fast.' >&2
    exit 2
fi

xwin_cache=${XWIN_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/cargo-xwin}
if [[ $toolchain == auto || ( -z $toolchain && $fast == 1 ) ]]; then
    toolchain=gnu
    if [[ -z $linker && -f $xwin_cache/xwin/DONE ]] && command -v cargo-xwin >/dev/null 2>&1 &&
            command -v clang >/dev/null 2>&1 && command -v lld-link >/dev/null 2>&1 &&
            command -v llvm-lib >/dev/null 2>&1 &&
            rustup target list --installed | grep -qx x86_64-pc-windows-msvc; then
        toolchain=msvc
    fi
fi
toolchain=${toolchain:-gnu}
if [[ $toolchain == msvc ]] && (( !features_set )); then
    features=native-web-preview
fi
case $toolchain in
    gnu) target=x86_64-pc-windows-gnu ;;
    msvc) target=x86_64-pc-windows-msvc ;;
    *) echo "--toolchain must be msvc, gnu or auto, not '$toolchain'." >&2; exit 2 ;;
esac
if [[ $toolchain == msvc && -n $linker ]]; then
    echo '--linker applies only to --toolchain gnu; msvc always links with lld-link.' >&2
    exit 2
fi
opt_level=${opt_level:-2}
ui_opt_level=${ui_opt_level:-0}
for level in "$opt_level" "$ui_opt_level"; do
    if [[ ! $level =~ ^[0-3]$ ]]; then
        echo "Optimization levels must be 0, 1, 2 or 3, not '$level'." >&2
        exit 2
    fi
done
if (( !make_zip )) && [[ -n $out_dir ]]; then
    echo '--out copies the ZIP; do not combine it with --no-zip.' >&2
    exit 2
fi

for tool in cargo rustup x86_64-w64-mingw32-gcc x86_64-w64-mingw32-ar x86_64-w64-mingw32-objdump x86_64-w64-mingw32-strip zip; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "Missing $tool. See scripts/build-windows.sh --help for setup." >&2
        exit 1
    fi
done
installed_targets=$(rustup target list --installed)
if [[ $'\n'"$installed_targets"$'\n' != *$'\n'"$target"$'\n'* ]]; then
    echo "Missing Rust target. Run: rustup target add $target" >&2
    exit 1
fi

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_dir"
python3 packaging/update-notices.py --check
CARGO_TARGET_DIR=$(realpath -m "${CARGO_TARGET_DIR:-$repo_dir/target}")
export CARGO_TARGET_DIR
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
export CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc
export AR_x86_64_pc_windows_gnu=x86_64-w64-mingw32-ar

if [[ $toolchain == msvc ]]; then
    for tool in cargo-xwin clang lld-link llvm-lib; do
        if ! command -v "$tool" >/dev/null 2>&1; then
            echo "Missing $tool, needed by --toolchain msvc." >&2
            exit 1
        fi
    done
    # Never trigger cargo-xwin's download of the Microsoft CRT and SDK: that
    # requires accepting Microsoft's licence, which the user does explicitly.
    if [[ ! -f $xwin_cache/xwin/DONE ]]; then
        echo "No cargo-xwin CRT/SDK cache in $xwin_cache/xwin." >&2
        echo 'Run cargo xwin once yourself (it downloads the Microsoft CRT and SDK under' >&2
        echo "Microsoft's licence), or use --toolchain gnu." >&2
        exit 1
    fi
    xwin_env=$(cargo xwin env --target "$target")
    eval "$xwin_env"
    # cargo-xwin exports an empty RUSTFLAGS, which would make Cargo ignore the
    # target-specific flags below.
    if [[ -z ${RUSTFLAGS:-} ]]; then unset RUSTFLAGS; fi
    # Link the C runtime statically, as the release build does, so the
    # executable does not need the Visual C++ redistributable.
    CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS="${CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS:-} -C target-feature=+crt-static"
    export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS
    linker=lld-link
elif (( fast )); then
    if [[ -z $linker ]]; then
        if command -v ld.lld >/dev/null 2>&1; then linker=lld; else linker=bfd; fi
    fi
    case $linker in
        lld)
            if ! command -v ld.lld >/dev/null 2>&1; then
                echo 'Missing ld.lld. Install lld or pass --linker bfd.' >&2
                exit 1
            fi
            ;;
        bfd) ;;
        *) echo "--linker must be lld or bfd, not '$linker'." >&2; exit 2 ;;
    esac
fi

if (( fast )); then
    profile=wintest
    # A profile defined on the command line, so Cargo.toml stays the single
    # source of the release settings. Dependencies are built once at full
    # optimization; workspace crates use the lower level and are compiled
    # incrementally, which pays off because the target directory persists.
    cargo_args=(
        --profile "$profile"
        --config "profile.$profile.inherits='release'"
        --config "profile.$profile.opt-level=$opt_level"
        --config "profile.$profile.lto='off'"
        --config "profile.$profile.codegen-units=4096"
        --config "profile.$profile.debug=false"
        --config "profile.$profile.strip='debuginfo'"
        --config "profile.$profile.incremental=true"
        --config "profile.$profile.package.'*'.opt-level=3"
        --config "profile.$profile.package.'*'.codegen-units=16"
    )
    # The UI crates split out of rustpy-ui keep its optimization level.
    for ui_crate in rustpy-ui rustpy-ui-base; do
        cargo_args+=(--config "profile.$profile.package.$ui_crate.opt-level=$ui_opt_level")
    done
    if [[ $linker == lld ]]; then
        # Scoped to the Windows target: build scripts and proc macros are unaffected.
        cargo_args+=(--config "target.$target.rustflags=['-C','link-arg=-fuse-ld=lld']")
    fi
    if [[ -n $features ]]; then
        cargo_args+=(--features "$features")
    fi
    package_dir="$CARGO_TARGET_DIR/windows-test"
    archive="$CARGO_TARGET_DIR/rustpy-windows-x86_64-test.zip"
    build_dir=$profile
    echo "Fast test build: $target, workspace opt-level $opt_level (rustpy-ui $ui_opt_level), no LTO, incremental, linker $linker${features:+, features $features}"
else
    cargo_args=(--release)
    package_dir="$CARGO_TARGET_DIR/windows-portable"
    archive="$CARGO_TARGET_DIR/rustpy-windows-x86_64.zip"
    build_dir=release
fi

build_start=$SECONDS
cargo build --locked "${cargo_args[@]}" -p rustpy --target "$target"
build_seconds=$((SECONDS - build_start))
package_start=$SECONDS

mkdir -p "$package_dir"
# Only remove files produced by this script, leaving other files alone.
rm -f "$package_dir/rustpy.exe" "$package_dir/libgcc_s_seh-1.dll" \
    "$package_dir/libstdc++-6.dll" "$package_dir/libwinpthread-1.dll" \
    "$package_dir/THIRD-PARTY-NOTICES.txt"
cp "$CARGO_TARGET_DIR/$target/$build_dir/rustpy.exe" "$package_dir/"
if (( !fast )); then
    # The fast profile already strips debug information while linking.
    x86_64-w64-mingw32-strip --strip-debug "$package_dir/rustpy.exe"
fi

if [[ $toolchain == msvc ]] && x86_64-w64-mingw32-objdump -p "$package_dir/rustpy.exe" |
        grep -qiE 'DLL Name: (vcruntime|msvcp|api-ms-win-crt-)'; then
    echo 'The executable imports the dynamic Visual C++ runtime; expected +crt-static.' >&2
    exit 1
fi

# Include MinGW runtime DLLs when imported, including their own dependencies.
files=("$package_dir/rustpy.exe")
for ((i = 0; i < ${#files[@]}; i++)); do
    imports=$(x86_64-w64-mingw32-objdump -p "${files[i]}" | awk '/DLL Name:/ {print $3}')
    while read -r dll; do
        case "${dll,,}" in
            libgcc_s_seh-1.dll|libstdc++-6.dll|libwinpthread-1.dll)
                if [[ ! -f "$package_dir/$dll" ]]; then
                    dll_path=$(x86_64-w64-mingw32-gcc -print-file-name="$dll")
                    if [[ ! -f "$dll_path" ]]; then
                        echo "Cannot locate required runtime DLL: $dll" >&2
                        exit 1
                    fi
                    cp "$dll_path" "$package_dir/$dll"
                    files+=("$package_dir/$dll")
                fi
                ;;
        esac
    done <<< "$imports"
done

# The retained licenses and notices travel beside the executable (packaging/README.md).
cp THIRD-PARTY-NOTICES.txt "$package_dir/"
cp LICENSE "$package_dir/"
files+=("$package_dir/LICENSE" "$package_dir/THIRD-PARTY-NOTICES.txt")

# Corresponding source and licence evidence for this exact executable: published beside the
# archive (THIRD-PARTY-NOTICES.txt names it), not inside it. It needs the link evidence of a
# release build (RUSTPY_NATIVE_RUNTIME_MANIFEST); without it this is a local test copy, which
# must not be redistributed.
rm -f "$package_dir/Native-Licenses.zip"
evidence=""
if (( !fast )) && [[ -n ${RUSTPY_NATIVE_RUNTIME_MANIFEST:-} ]]; then
    evidence="${archive%.zip}-native-licenses.zip"
    python3 packaging/stage-native-licenses.py --binary "$package_dir/rustpy.exe" --runtime-root "$package_dir" --output "$evidence"
elif (( !fast )); then
    echo "build-windows: RUSTPY_NATIVE_RUNTIME_MANIFEST is not set, so no licence evidence bundle was made: do not redistribute this build" >&2
fi

if (( make_zip )); then
    rm -f "$archive"
    if (( fast )); then
        # Fastest compression: this is a local test copy, not a download.
        (cd "$package_dir" && zip -q -1 -j "$archive" "${files[@]}")
    else
        (cd "$package_dir" && zip -j "$archive" "${files[@]}")
    fi
fi

# A path Windows Explorer can open (\\wsl.localhost\...) when running under WSL.
windows_path() {
    if [[ -n ${WSL_DISTRO_NAME:-} ]] && command -v wslpath >/dev/null 2>&1; then
        wslpath -w "$1" 2>/dev/null || true
    fi
}

echo "Windows executable: $package_dir/rustpy.exe"
win_path=$(windows_path "$package_dir/rustpy.exe")
[[ -z $win_path ]] || echo "  from Windows: $win_path"
if (( make_zip )); then
    echo "Portable ZIP: $archive"
    win_path=$(windows_path "$archive")
    [[ -z $win_path ]] || echo "  from Windows: $win_path"
fi
[[ -z $evidence ]] || echo "Licence evidence: $evidence"
if [[ -n $out_dir ]]; then
    mkdir -p "$out_dir"
    cp "$archive" "$out_dir/"
    copied="$(realpath "$out_dir")/$(basename "$archive")"
    echo "Copied ZIP: $copied"
    win_path=$(windows_path "$copied")
    [[ -z $win_path ]] || echo "  from Windows: $win_path"
fi
echo "Build ${build_seconds}s, packaging $((SECONDS - package_start))s"
echo 'Imported DLLs:'
x86_64-w64-mingw32-objdump -p "$package_dir/rustpy.exe" | awk '/DLL Name:/ {print "  " $3}'
