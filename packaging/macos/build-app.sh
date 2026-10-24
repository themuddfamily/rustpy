#!/usr/bin/env bash
# Builds RustPy.app from the release binary and packs it into a .dmg (run on macOS).
#
# Usage: packaging/macos/build-app.sh [--no-build] [--universal] [--no-dmg] [--icon FILE] [--out DIR]
#   --no-build   package existing release binaries instead of running cargo build first
#   --universal  one arm64 + x86_64 binary (lipo); needs both Rust targets:
#                rustup target add aarch64-apple-darwin x86_64-apple-darwin
#   --no-dmg     stop after RustPy.app
#   --icon FILE  use a ready-made .icns instead of rendering packaging/linux/rustpy.svg
#   --out DIR    output directory (default: <target dir>/macos)
#
# Output: DIR/RustPy.app and DIR/RustPy-<version>-<arch>.dmg (arch: arm64, x86_64, universal).
#
# The bundle holds Contents/MacOS/rustpy, Contents/bin/rustpy (a command-line launcher
# script to link onto PATH), Contents/PkgInfo, Contents/Info.plist (bundle id, version from
# Cargo.toml, CFBundleIconFile=rustpy, document types for .py, .pyi and .ipynb so that
# Finder's "Open With" lists RustPy) and Contents/Resources/rustpy.icns, rendered from the
# SVG with rsvg-convert, magick or qlmanage plus sips and iconutil. Where that is impossible
# (e.g. trying the script on Linux) the SVG is stored as a placeholder and Finder shows the
# generic icon until a Resources/rustpy.icns is added.
#
# The .dmg comes from create-dmg (`brew install create-dmg`: Finder layout with an
# Applications drop link; it scripts Finder unless CI is set) or else from hdiutil.
#
# Environment:
#   CARGO_TARGET_DIR   where cargo puts the binaries (default: target)
#   BUNDLE_ID          CFBundleIdentifier (default: dev.rustpy.RustPy)
#   CODESIGN_IDENTITY  "Developer ID Application: ..." signs the app (hardened runtime) and
#                      the .dmg; without it the app is ad-hoc signed
#   NOTARY_PROFILE     `xcrun notarytool store-credentials` profile; notarizes and staples
#                      the .dmg (needs CODESIGN_IDENTITY)
set -euo pipefail

usage() { sed -n '2,/^set -euo pipefail$/{/^set /d;s/^# \{0,1\}//;p;}' "$0"; }
die() { echo "build-app: $*" >&2; exit 1; }

BUILD=1
UNIVERSAL=0
DMG=1
ICON=""
OUT=""
while (($#)); do
  case "$1" in
    --no-build) BUILD=0 ;;
    --universal) UNIVERSAL=1 ;;
    --no-dmg) DMG=0 ;;
    --icon) (($# >= 2)) || die "--icon needs a file"; ICON="$2"; shift ;;
    --icon=*) ICON="${1#--icon=}" ;;
    --out) (($# >= 2)) || die "--out needs a directory"; OUT="$2"; shift ;;
    --out=*) OUT="${1#--out=}" ;;
    -h|--help) usage; exit 0 ;;
    *) echo "build-app: unknown option: $1 (see --help)" >&2; exit 2 ;;
  esac
  shift
done
if [[ -n $ICON ]]; then
  [[ -f $ICON ]] || die "icon not found: $ICON"
  ICON="$(cd -- "$(dirname -- "$ICON")" && pwd)/$(basename -- "$ICON")"
fi
if [[ -n $OUT ]]; then
  mkdir -p -- "$OUT"
  OUT="$(cd -- "$OUT" && pwd)"
fi

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
python3 packaging/update-notices.py --check

TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
OUT="${OUT:-$TARGET_DIR/macos}"
BUNDLE_ID="${BUNDLE_ID:-dev.rustpy.RustPy}"
[[ $BUNDLE_ID =~ ^[A-Za-z0-9.-]+$ ]] || die "BUNDLE_ID may only contain letters, digits, '.' and '-'"
if [[ -n ${NOTARY_PROFILE:-} && -z ${CODESIGN_IDENTITY:-} ]]; then
  die "NOTARY_PROFILE needs CODESIGN_IDENTITY (notarization requires a Developer ID signature)"
fi
VERSION="$(awk -F'"' '/^\[/ { section = $0 } section == "[workspace.package]" && $1 ~ /^version *= *$/ { print $2; exit }' Cargo.toml)"
[[ -n $VERSION ]] || die "cannot read the [workspace.package] version from Cargo.toml"
# Both Apple version fields use the numeric SemVer core, never "-beta.1" / "+build".
BUNDLE_VERSION="${VERSION%%[-+]*}"
[[ $BUNDLE_VERSION =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "bundle version must have three numeric components: $VERSION"
SVG="$ROOT/packaging/linux/rustpy.svg"
APP="$OUT/RustPy.app"
mkdir -p -- "$OUT"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/rustpy-app.XXXXXX")"
trap 'rm -rf -- "$WORK"' EXIT

# --- 1. binary --------------------------------------------------------------
if ((UNIVERSAL)); then
  if ((BUILD)); then
    cargo build --locked --release -p rustpy --features "${RUSTPY_RELEASE_FEATURES-native-web-preview}" --target aarch64-apple-darwin
    cargo build --locked --release -p rustpy --features "${RUSTPY_RELEASE_FEATURES-native-web-preview}" --target x86_64-apple-darwin
  fi
  command -v lipo >/dev/null 2>&1 || die "lipo not found (Xcode command line tools)"
  BIN="$WORK/rustpy"
  lipo -create -output "$BIN" \
    "$TARGET_DIR/aarch64-apple-darwin/release/rustpy" \
    "$TARGET_DIR/x86_64-apple-darwin/release/rustpy"
  lipo -verify_arch arm64 x86_64 "$BIN" || die "lipo did not produce an arm64 + x86_64 binary"
  echo "Universal binary: $(lipo -archs "$BIN")"
  ARCH=universal
else
  if ((BUILD)); then
    cargo build --locked --release -p rustpy --features "${RUSTPY_RELEASE_FEATURES-native-web-preview}"
  fi
  BIN="$TARGET_DIR/release/rustpy"
  ARCH="$(uname -m)"
fi
[[ -x $BIN ]] || die "release binary not found: $BIN (run without --no-build)"

# --- 2. bundle --------------------------------------------------------------
rm -rf -- "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/rustpy"
cp THIRD-PARTY-NOTICES.txt "$APP/Contents/Resources/THIRD-PARTY-NOTICES.txt"
cp LICENSE "$APP/Contents/Resources/LICENSE"
chmod 755 "$APP/Contents/MacOS/rustpy"
printf 'APPL????' > "$APP/Contents/PkgInfo"
# Command-line launcher in Contents/bin: link it onto PATH
# (ln -s /Applications/RustPy.app/Contents/bin/rustpy /usr/local/bin/rustpy). It runs the
# app's binary with the terminal's environment, so `rustpy .` opens the current folder and
# finds the interpreters on the shell's PATH.
mkdir -p "$APP/Contents/bin"
cat > "$APP/Contents/bin/rustpy" <<'LAUNCHER'
#!/bin/sh
# rustpy: command-line launcher of RustPy.app. Usage: rustpy [options] [folder | file]
# Runs Contents/MacOS/rustpy of the bundle this script is in (symlinks are followed).
self="$0"
while [ -L "$self" ]; do
  link="$(readlink "$self")"
  case "$link" in
    /*) self="$link" ;;
    *) self="$(dirname "$self")/$link" ;;
  esac
done
contents="$(cd "$(dirname "$self")/.." && pwd -P)"
exec "$contents/MacOS/rustpy" "$@"
LAUNCHER
chmod 755 "$APP/Contents/bin/rustpy"

# --- 3. icon ----------------------------------------------------------------
make_icns() { # $1: output .icns
  local png="$WORK/icon-1024.png" iconset="$WORK/rustpy.iconset" size
  if command -v rsvg-convert >/dev/null 2>&1; then
    rsvg-convert -w 1024 -h 1024 -o "$png" "$SVG" || return 1
  elif command -v magick >/dev/null 2>&1; then
    magick -background none -density 384 "$SVG" -resize 1024x1024 "$png" || return 1
  elif command -v qlmanage >/dev/null 2>&1; then
    qlmanage -t -s 1024 -o "$WORK" "$SVG" >/dev/null 2>&1 || return 1
    mv -f "$WORK/rustpy.svg.png" "$png" || return 1
  else
    return 1
  fi
  { command -v sips && command -v iconutil; } >/dev/null 2>&1 || return 1
  mkdir -p "$iconset" || return 1
  for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$png" --out "$iconset/icon_${size}x${size}.png" >/dev/null || return 1
    sips -z $((size * 2)) $((size * 2)) "$png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null || return 1
  done
  iconutil -c icns "$iconset" -o "$1"
}
if [[ -n $ICON ]]; then
  cp "$ICON" "$APP/Contents/Resources/rustpy.icns"
elif ! make_icns "$APP/Contents/Resources/rustpy.icns"; then
  rm -f -- "$APP/Contents/Resources/rustpy.icns"
  cp "$SVG" "$APP/Contents/Resources/rustpy.svg"
  echo "build-app: note: cannot render rustpy.icns here (needs sips and iconutil); stored rustpy.svg as a placeholder" >&2
fi

# --- 4. Info.plist ----------------------------------------------------------
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleDisplayName</key>
	<string>RustPy</string>
	<key>CFBundleExecutable</key>
	<string>rustpy</string>
	<key>CFBundleIconFile</key>
	<string>rustpy</string>
	<key>CFBundleIdentifier</key>
	<string>${BUNDLE_ID}</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>RustPy</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>${BUNDLE_VERSION}</string>
	<key>CFBundleVersion</key>
	<string>${BUNDLE_VERSION}</string>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.developer-tools</string>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSSupportsAutomaticGraphicsSwitching</key>
	<true/>
	<key>NSHumanReadableCopyright</key>
	<string>Copyright 2026 RustPy contributors. Apache License 2.0.</string>
	<key>CFBundleDocumentTypes</key>
	<array>
		<dict>
			<key>CFBundleTypeName</key>
			<string>Python Source</string>
			<key>CFBundleTypeRole</key>
			<string>Editor</string>
			<key>LSHandlerRank</key>
			<string>Alternate</string>
			<key>LSItemContentTypes</key>
			<array>
				<string>public.python-script</string>
			</array>
		</dict>
		<dict>
			<key>CFBundleTypeName</key>
			<string>Python Stub</string>
			<key>CFBundleTypeRole</key>
			<string>Editor</string>
			<key>LSHandlerRank</key>
			<string>Alternate</string>
			<key>LSItemContentTypes</key>
			<array>
				<string>dev.rustpy.python-stub</string>
			</array>
		</dict>
		<dict>
			<key>CFBundleTypeName</key>
			<string>Jupyter Notebook</string>
			<key>CFBundleTypeRole</key>
			<string>Editor</string>
			<key>LSHandlerRank</key>
			<string>Alternate</string>
			<key>LSItemContentTypes</key>
			<array>
				<string>org.jupyter.ipynb</string>
			</array>
		</dict>
	</array>
	<key>UTImportedTypeDeclarations</key>
	<array>
		<dict>
			<key>UTTypeIdentifier</key>
			<string>dev.rustpy.python-stub</string>
			<key>UTTypeDescription</key>
			<string>Python Stub</string>
			<key>UTTypeConformsTo</key>
			<array>
				<string>public.python-script</string>
			</array>
			<key>UTTypeTagSpecification</key>
			<dict>
				<key>public.filename-extension</key>
				<array>
					<string>pyi</string>
				</array>
			</dict>
		</dict>
		<dict>
			<key>UTTypeIdentifier</key>
			<string>org.jupyter.ipynb</string>
			<key>UTTypeDescription</key>
			<string>Jupyter Notebook</string>
			<key>UTTypeConformsTo</key>
			<array>
				<string>public.json</string>
			</array>
			<key>UTTypeTagSpecification</key>
			<dict>
				<key>public.filename-extension</key>
				<array>
					<string>ipynb</string>
				</array>
				<key>public.mime-type</key>
				<array>
					<string>application/x-ipynb+json</string>
				</array>
			</dict>
		</dict>
	</array>
</dict>
</plist>
PLIST
if command -v plutil >/dev/null 2>&1; then
  plutil -lint "$APP/Contents/Info.plist" >/dev/null
fi

python3 packaging/stage-native-licenses.py --binary-stage pre-sign --binary "$APP/Contents/MacOS/rustpy" --runtime-root "$APP" --output "$APP/Contents/Resources/Native-Licenses.zip"

# --- 5. signing -------------------------------------------------------------
if command -v codesign >/dev/null 2>&1; then
  if [[ -n ${CODESIGN_IDENTITY:-} ]]; then
    codesign --force --options runtime --timestamp --sign "$CODESIGN_IDENTITY" "$APP"
  else
    # Ad-hoc signature: Apple Silicon refuses to run unsigned code.
    codesign --force --sign - "$APP"
  fi
else
  echo "build-app: note: codesign not found (not macOS?); RustPy.app is unsigned" >&2
fi
echo "App bundle: $APP"

# --- 6. disk image ----------------------------------------------------------
((DMG)) || exit 0
DMGFILE="$OUT/RustPy-$VERSION-$ARCH.dmg"
STAGE="$WORK/dmg"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
rm -f -- "$DMGFILE"
DMG_TOOL=""
if command -v create-dmg >/dev/null 2>&1; then
  case "$(create-dmg --help 2>&1 || true)" in
    *--app-drop-link*) DMG_TOOL=create-dmg ;; # create-dmg/create-dmg, not the npm package
  esac
fi
if [[ -z $DMG_TOOL ]] && command -v hdiutil >/dev/null 2>&1; then
  DMG_TOOL=hdiutil
fi
if [[ $DMG_TOOL == create-dmg ]]; then
  DMG_ARGS=(--volname RustPy --window-size 560 340 --icon-size 96
    --icon RustPy.app 140 160 --hide-extension RustPy.app --app-drop-link 420 160)
  if [[ -f $APP/Contents/Resources/rustpy.icns ]]; then
    DMG_ARGS+=(--volicon "$APP/Contents/Resources/rustpy.icns")
  fi
  if [[ -n ${CI:-} ]]; then
    DMG_ARGS+=(--skip-jenkins) # no Finder scripting on headless runners
  fi
  create-dmg "${DMG_ARGS[@]}" "$DMGFILE" "$STAGE"
elif [[ $DMG_TOOL == hdiutil ]]; then
  ln -s /Applications "$STAGE/Applications"
  hdiutil create -volname RustPy -srcfolder "$STAGE" -ov -format UDZO "$DMGFILE"
else
  echo "build-app: note: neither create-dmg nor hdiutil found (not macOS?); skipping the .dmg" >&2
  exit 0
fi
if [[ -n ${CODESIGN_IDENTITY:-} ]]; then
  codesign --force --timestamp --sign "$CODESIGN_IDENTITY" "$DMGFILE"
fi
if [[ -n ${NOTARY_PROFILE:-} ]]; then
  xcrun notarytool submit "$DMGFILE" --keychain-profile "$NOTARY_PROFILE" --wait
  xcrun stapler staple "$DMGFILE"
fi
echo "Disk image: $DMGFILE"
