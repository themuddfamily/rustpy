#!/usr/bin/env bash
# Cross-compile and package RustPy using Linux tools only.
set -euo pipefail

if [[ ${1:-} == --help || ${1:-} == -h ]]; then
    cat <<'EOF'
Usage: scripts/build-windows.sh

Builds a native 64-bit Windows executable and portable ZIP from WSL/Linux.
No Windows installation, SDK, or Visual Studio is needed.

One-time setup on Ubuntu/Debian:
  sudo apt-get update
  sudo apt-get install -y mingw-w64 zip
  rustup target add x86_64-pc-windows-gnu

Output (under CARGO_TARGET_DIR, or target/ by default):
  windows-portable/rustpy.exe
  rustpy-windows-x86_64.zip

Extract the ZIP and run rustpy.exe on Windows. Python is needed to run Python
projects. Settings are stored in the user configuration directory; this does
not bundle Python or make settings travel with the executable.
EOF
    exit 0
fi
if (( $# != 0 )); then
    echo 'Usage: scripts/build-windows.sh [--help]' >&2
    exit 2
fi

for tool in cargo rustup x86_64-w64-mingw32-gcc x86_64-w64-mingw32-ar x86_64-w64-mingw32-objdump x86_64-w64-mingw32-strip zip; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "Missing $tool. See scripts/build-windows.sh --help for setup." >&2
        exit 1
    fi
done
target=x86_64-pc-windows-gnu
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

cargo build --locked --release -p rustpy --target "$target"

package_dir="$CARGO_TARGET_DIR/windows-portable"
archive="$CARGO_TARGET_DIR/rustpy-windows-x86_64.zip"
mkdir -p "$package_dir"
# Only remove files produced by this script, leaving other files alone.
rm -f "$package_dir/rustpy.exe" "$package_dir/libgcc_s_seh-1.dll" \
    "$package_dir/libstdc++-6.dll" "$package_dir/libwinpthread-1.dll" \
    "$package_dir/THIRD-PARTY-NOTICES.txt"
cp "$CARGO_TARGET_DIR/$target/release/rustpy.exe" "$package_dir/"
x86_64-w64-mingw32-strip --strip-debug "$package_dir/rustpy.exe"

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

python3 packaging/stage-native-licenses.py --binary "$package_dir/rustpy.exe" --runtime-root "$package_dir" --output "$package_dir/Native-Licenses.zip"
files+=("$package_dir/Native-Licenses.zip")

rm -f "$archive"
(cd "$package_dir" && zip -j "$archive" "${files[@]}")
echo "Windows executable: $package_dir/rustpy.exe"
echo "Portable ZIP: $archive"
echo 'Imported DLLs:'
x86_64-w64-mingw32-objdump -p "$package_dir/rustpy.exe" | awk '/DLL Name:/ {print "  " $3}'
