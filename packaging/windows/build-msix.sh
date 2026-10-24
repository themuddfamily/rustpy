#!/usr/bin/env bash
# Builds RustPy MSIX packages (Microsoft Store / sideloading) on Linux or WSL from
# cross-built rustpy.exe files, with Microsoft's makemsix (msix-packaging SDK).
#
# Usage: packaging/windows/build-msix.sh [options]
#   --arch x64|arm64     package this architecture (repeatable; default: every arch whose
#                        rustpy.exe exists under $CARGO_TARGET_DIR/<triple>/release)
#   --exe ARCH=PATH      use this rustpy.exe for ARCH instead (e.g. --exe x64=/x/rustpy.exe)
#   --out DIR            output directory (default: $CARGO_TARGET_DIR/msix)
#   --sign               also write *-test-signed.msix signed with a throw-away self-signed
#                        certificate whose subject is the manifest Publisher (local testing
#                        only; the Store signs submissions itself) and export it as
#                        rustpy-msix-test.cer next to the packages
#   --build-makemsix     clone and build makemsix into $MSIX_TOOLS_DIR when it is missing
#   -h | --help
#
# No .msixbundle here: makemsix on Linux only writes flat bundles (references to external
# packages). build-msix.ps1 -Bundle (makeappx on Windows, as in release.yml) writes a real
# bundle; Partner Center equally accepts the per-architecture .msix files in one submission.
#
# Inputs (cross-build them first, see packaging/README.md):
#   x64:   cargo xwin build --release --target x86_64-pc-windows-msvc -p rustpy --features native-web-preview
#   arm64: packaging/windows/cross-build-arm64.sh
# Tools: makemsix ($MAKEMSIX, PATH, or $MSIX_TOOLS_DIR/msix-packaging/.vs/bin/makemsix;
#        default MSIX_TOOLS_DIR=$CARGO_TARGET_DIR/msix-tools); --sign needs openssl and
#        osslsigncode >= 2.6 (apt install osslsigncode).
# Identity placeholders: packaging/windows/msix/identity.env, overridable per value with
# RUSTPY_MSIX_IDENTITY_NAME, RUSTPY_MSIX_PUBLISHER, RUSTPY_MSIX_PUBLISHER_DISPLAY_NAME and
# RUSTPY_MSIX_DISPLAY_NAME.
#
# Outputs: rustpy-<version>-windows-<arch>.msix (unsigned: upload these to Partner Center),
# and with --sign *-test-signed.msix and rustpy-msix-test.cer.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
target_dir="$(realpath -m "${CARGO_TARGET_DIR:-$root/target}")"
out="$target_dir/msix"
archs=()
declare -A exes=()
sign=0 build_tool=0

while [ $# -gt 0 ]; do
    case "$1" in
        --arch) archs+=("$2"); shift ;;
        --exe) exes["${2%%=*}"]="${2#*=}"; shift ;;
        --out) out="$(realpath -m "$2")"; shift ;;
        --sign) sign=1 ;;
        --build-makemsix) build_tool=1 ;;
        -h|--help) sed -n '2,35p' "$0"; exit 0 ;;
        *) echo "build-msix: unknown option $1" >&2; exit 2 ;;
    esac
    shift
done

python3 "$root/packaging/update-notices.py" --check

triple() { case "$1" in x64) echo x86_64-pc-windows-msvc ;; arm64) echo aarch64-pc-windows-msvc ;; *) echo "build-msix: unsupported arch $1" >&2; exit 2 ;; esac; }
exe_for() { if [ -n "${exes[$1]:-}" ]; then echo "${exes[$1]}"; else echo "$target_dir/$(triple "$1")/release/rustpy.exe"; fi; }
if [ ${#archs[@]} -eq 0 ]; then
    for a in x64 arm64; do [ -f "$(exe_for "$a")" ] && archs+=("$a"); done
    for a in "${!exes[@]}"; do [[ " ${archs[*]} " == *" $a "* ]] || archs+=("$a"); done
fi
[ ${#archs[@]} -gt 0 ] || { echo "build-msix: no rustpy.exe found; cross-build it first or pass --exe" >&2; exit 1; }

# --- version: workspace version, prerelease/build suffix dropped, four parts, last 0 ---
cargo_version="$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version = "\(.*\)"/\1/p}' "$root/Cargo.toml" | head -n1)"
base="${cargo_version%%[-+]*}"
IFS=. read -r v1 v2 v3 <<<"$base"
msix_version="${v1:-0}.${v2:-0}.${v3:-0}.0"

# --- identity (placeholders until Partner Center) ---
declare -A id=()
while IFS= read -r line; do
    line="${line%$'\r'}"
    [[ -z "$line" || "$line" == \#* || "$line" != *=* ]] && continue
    id["${line%%=*}"]="${line#*=}"
done < "$here/msix/identity.env"
for key in IDENTITY_NAME PUBLISHER PUBLISHER_DISPLAY_NAME DISPLAY_NAME; do
    var="RUSTPY_MSIX_$key"
    [ -n "${!var:-}" ] && id[$key]="${!var}"
    [ -n "${id[$key]:-}" ] || { echo "build-msix: $key is empty" >&2; exit 1; }
done
case "${id[PUBLISHER]}" in CN=*) ;; *) echo "build-msix: PUBLISHER must start with CN=" >&2; exit 1 ;; esac

# --- makemsix ---
tools="${MSIX_TOOLS_DIR:-$target_dir/msix-tools}"
makemsix="${MAKEMSIX:-$(command -v makemsix || true)}"
[ -n "$makemsix" ] || [ ! -x "$tools/msix-packaging/.vs/bin/makemsix" ] || makemsix="$tools/msix-packaging/.vs/bin/makemsix"
if [ -z "$makemsix" ]; then
    [ "$build_tool" = 1 ] || { echo "build-msix: makemsix not found; set MAKEMSIX or rerun with --build-makemsix" >&2; exit 1; }
    mkdir -p "$tools"
    [ -d "$tools/msix-packaging" ] || git clone --depth 1 https://github.com/microsoft/msix-packaging "$tools/msix-packaging"
    cmake_bin="$(command -v cmake)"
    # msix-packaging needs CMake >= 3.29; use a private pip copy when the system one is older.
    if ! "$cmake_bin" --version | awk 'NR==1{split($3,v,"."); exit !(v[1]>3 || (v[1]==3 && v[2]>=29))}'; then
        python3 -m venv "$tools/venv" && "$tools/venv/bin/pip" install -q cmake
        cmake_bin="$tools/venv/bin/cmake"
    fi
    mkdir -p "$tools/msix-packaging/.vs"
    (cd "$tools/msix-packaging/.vs" &&
        "$cmake_bin" -DCMAKE_POLICY_VERSION_MINIMUM=3.5 -DCMAKE_BUILD_TYPE=MinSizeRel -DSKIP_BUNDLES=off \
            -DUSE_VALIDATION_PARSER=on -DCMAKE_TOOLCHAIN_FILE=../cmake/linux.cmake -DMSIX_PACK=on \
            -DMSIX_SAMPLES=off -DMSIX_TESTS=off -DLINUX=on .. >/dev/null &&
        make -j"${CARGO_BUILD_JOBS:-2}" makemsix >/dev/null)
    makemsix="$tools/msix-packaging/.vs/bin/makemsix"
fi

xml_escape() { local s="$1"; s="${s//&/&amp;}"; s="${s//</&lt;}"; s="${s//>/&gt;}"; s="${s//\"/&quot;}"; printf '%s' "$s"; }
# sed replacement text: escape \, & and the | delimiter.
sed_escape() { printf '%s' "$1" | sed -e 's/[\\&|]/\\&/g'; }

mkdir -p "$out"
packages=()
for arch in "${archs[@]}"; do
    exe="$(exe_for "$arch")"
    [ -f "$exe" ] || { echo "build-msix: $exe not found" >&2; exit 1; }
    stage="$out/stage-$arch"
    rm -rf "$stage"
    mkdir -p "$stage/Assets"
    cp "$exe" "$stage/rustpy.exe"
    cp "$root/THIRD-PARTY-NOTICES.txt" "$stage/"
    cp "$root/LICENSE" "$stage/"
    # Fallback install-kind marker; package identity (GetCurrentPackageFullName) wins.
    printf 'msix\r\n' > "$stage/rustpy-install.kind"
    cp "$here/msix/Assets/"*.png "$stage/Assets/"
    sed -e "s|@IDENTITY_NAME@|$(sed_escape "$(xml_escape "${id[IDENTITY_NAME]}")")|g" \
        -e "s|@PUBLISHER@|$(sed_escape "$(xml_escape "${id[PUBLISHER]}")")|g" \
        -e "s|@PUBLISHER_DISPLAY_NAME@|$(sed_escape "$(xml_escape "${id[PUBLISHER_DISPLAY_NAME]}")")|g" \
        -e "s|@DISPLAY_NAME@|$(sed_escape "$(xml_escape "${id[DISPLAY_NAME]}")")|g" \
        -e "s|@VERSION@|$msix_version|g" -e "s|@ARCH@|$arch|g" \
        "$here/msix/AppxManifest.xml.in" > "$stage/AppxManifest.xml"
    if grep -q '@[A-Z_]*@' "$stage/AppxManifest.xml"; then echo "build-msix: unsubstituted placeholder" >&2; exit 1; fi
    package="$out/rustpy-$cargo_version-windows-$arch.msix"
    rm -f "$package"
    python3 "$root/packaging/stage-native-licenses.py" --binary "$stage/rustpy.exe" --runtime-root "$stage" --output "$stage/Native-Licenses.zip"
    "$makemsix" pack -d "$stage" -p "$package" >/dev/null
    echo "built $package ($msix_version, ${id[IDENTITY_NAME]}, ${id[PUBLISHER]})"
    packages+=("$package")
done

if [ "$sign" = 1 ]; then
    command -v osslsigncode >/dev/null || { echo "build-msix: --sign needs osslsigncode (apt install osslsigncode)" >&2; exit 1; }
    key="$out/rustpy-msix-test.key" crt="$out/rustpy-msix-test.pem" cer="$out/rustpy-msix-test.cer"
    # Reuse the certificate while its subject still matches the Publisher.
    subject="/$(printf '%s' "${id[PUBLISHER]}" | sed -e 's/, */\//g')"
    if [ ! -f "$key" ] || [ ! -f "$crt" ] || [ "$(cat "$out/rustpy-msix-test.subject" 2>/dev/null)" != "${id[PUBLISHER]}" ]; then
        openssl req -x509 -newkey rsa:3072 -sha256 -nodes -days 365 -keyout "$key" -out "$crt" \
            -subj "$subject" \
            -addext "basicConstraints=critical,CA:FALSE" \
            -addext "keyUsage=critical,digitalSignature" \
            -addext "extendedKeyUsage=codeSigning" 2>/dev/null
        printf '%s' "${id[PUBLISHER]}" > "$out/rustpy-msix-test.subject"
    fi
    openssl x509 -in "$crt" -outform DER -out "$cer"
    for package in "${packages[@]}"; do
        signed="${package%.msix}-test-signed.msix"
        rm -f "$signed"
        osslsigncode sign -certs "$crt" -key "$key" -h sha256 -in "$package" -out "$signed" >/dev/null
        echo "signed $signed"
    done
    echo "test certificate: $cer (import into Cert:\\CurrentUser\\TrustedPeople or LocalMachine\\TrustedPeople to install)"
fi
