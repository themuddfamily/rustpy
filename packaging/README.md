# Packaging and distribution

How to turn the workspace into installable release artifacts (roadmap §8, "Cross-platform
builds"). Every artifact takes its version from `[workspace.package] version` in the root
`Cargo.toml`, so bump it there before tagging a release. Where the IDE keeps settings, logs
and per-project files is described in
[docs/configuration-directories.md](../docs/configuration-directories.md).

| Platform | Artifact | Built by |
|---|---|---|
| Linux (most distributions) | `RustPy-<version>-<arch>.AppImage` | `packaging/linux/build-appimage.sh` |
| Debian, Ubuntu | `rustpy_<version>-1_<arch>.deb` | `packaging/linux/build-deb.sh` (cargo-deb) |
| Linux, from a checkout | `~/.local/bin/rustpy`, desktop entry and icon | `scripts/install.sh` |
| Windows | `rustpy-<version>-windows-<arch>-setup.exe`, `…-portable.zip` | `packaging/windows/build.ps1` (Inno Setup) |
| Windows, cross-built on Linux/WSL | `rustpy-windows-x86_64.zip` | `scripts/build-windows.sh` ([WINDOWS.md](../WINDOWS.md)) |
| macOS | `RustPy.app`, `RustPy-<version>-<arch>.dmg` | `packaging/macos/build-app.sh` |
| Fedora, RHEL, openSUSE | `rustpy-<version>-1.<arch>.rpm` | `packaging/linux/build-rpm.sh` (cargo-generate-rpm) |
| Linux | `rustpy-<version>-linux-<arch>.tar.gz` (the updater's archive layout) | `packaging/linux/build-tar.sh` |
| Linux sandboxes | Flatpak bundle, classic Snap | `packaging/linux/flatpak/build-flatpak.sh`, `packaging/linux/build-snap.sh` |
| Windows | `rustpy-<version>-windows-<arch>.msi` | `packaging/windows/build-msi.ps1` (WiX v4/v5) |
| Windows, Microsoft Store | `rustpy-<version>-windows-<arch>.msix`, `rustpy-<version>-windows.msixbundle` | `packaging/windows/build-msix.sh` (makemsix, Linux/WSL) or `build-msix.ps1` (makeappx, Windows) |
| Linux, macOS, Windows | all of the above, per architecture | `.github/workflows/release.yml` (manual dispatch; publishing is an opt-in input) |

```
packaging/
├── README.md               this file
├── linux/
│   ├── rustpy.desktop      desktop entry (Exec=rustpy %f, Icon=rustpy, StartupWMClass=rustpy)
│   ├── rustpy.svg          the logo, a snake-like S in a rounded square; source of every icon
│   ├── build-appimage.sh   AppDir + appimagetool
│   └── build-deb.sh        cargo-deb wrapper; the package metadata is in crates/rustpy/Cargo.toml
├── windows/
│   ├── rustpy.iss          Inno Setup script
│   ├── build.ps1           release build, icon, portable zip and installer
│   ├── rustpy.wxs          WiX v4/v5 MSI source (per user, feature tree)
│   ├── build-msi.ps1       MSI build (needs WiX and WixToolset.UI.wixext)
│   ├── msix/               MSIX manifest template, identity placeholders, logos (Assets/)
│   ├── build-msix.sh       MSIX from cross-built exes with makemsix; test signing with osslsigncode
│   └── build-msix.ps1      MSIX and .msixbundle with makeappx; test signing with signtool
└── macos/
    └── build-app.sh        RustPy.app (Info.plist, icon), signing, .dmg, notarization
scripts/install.sh          per-user install from source (Linux)
```

Every script prints its usage with `--help` (`Get-Help packaging\windows\build.ps1` on Windows).

## What the binary needs at run time

`rustpy` links unixODBC (`libodbc.so.2`) as well as its C/runtime libraries.
Other linked dependencies depend on the enabled features; inspect the candidate
binary's dynamic dependencies before packaging. It needs a glibc at least as new
as the build machine's (2.39 when built on Ubuntu 24.04), so build Linux packages on
the oldest distribution you want to support. The windowing libraries are
opened at run time by winit and glutin: `libxkbcommon.so.0`; on X11 `libxkbcommon-x11.so.0`,
`libX11`, `libX11-xcb`, `libxcb`, `libXcursor`, `libXi` and `libXrender`; on Wayland
`libwayland-client` and `libwayland-egl`; and `libEGL` or `libGL`. Desktop systems have them
all. Python, Git and the other tools the IDE drives are found at run time, never bundled.

The release profile keeps line-level debug info (`debug = 1`), so the scripts strip the
packaged binary (about 850 MB before, about 180 MB after `--strip-debug`; the compressed
packages are 35 to 60 MB): `strip --strip-debug` for the
AppImage and `install.sh`, which keeps symbol names for backtraces, and cargo-deb's
`--strip-unneeded` for the `.deb`. Pass `--no-strip` to keep everything.

## Linux

You need a Rust toolchain at least as new as `rust-version` in `Cargo.toml`, the development
headers (`libxkbcommon-dev libwayland-dev libgtk-3-dev unixodbc-dev`), `dpkg-dev` for the
`.deb`, and `curl` or `wget` to download appimagetool.

The mandatory database backend links unixODBC independently of
`native-web-preview`. On Debian/Ubuntu, `unixodbc-dev` supplies the build-time
headers and unversioned `libodbc.so` linker library; prebuilt executables require
`libodbc2`, which supplies `libodbc.so.2`. The runtime package alone cannot fix
`cannot find -lodbc`. On RPM-based systems the corresponding development package
is `unixODBC-devel`, and the installed runtime must provide `libodbc.so.2`.
Database-specific ODBC drivers are not included by installing the driver manager.

AppImage, tar archives and checkout/user-prefix installs do not bundle unixODBC;
install the host runtime before launching them (Debian/Ubuntu: `libodbc2`). The
Snap recipe explicitly builds against `unixodbc-dev` and stages `libodbc2`.
The GNOME runtime and SDK do not ship unixODBC, so the Flatpak manifest builds it
(2.3.12) as its own module into `/app/lib`; its configuration files are read from
`/app/etc`, and the host's `/etc/odbcinst.ini` and drivers are not visible in the sandbox.

### AppImage

```sh
packaging/linux/build-appimage.sh              # cargo build --locked --release -p rustpy, then package
packaging/linux/build-appimage.sh --no-build   # package the existing target/release/rustpy
packaging/linux/build-appimage.sh --out dist   # write to dist/ instead of target/appimage/
```

The script assembles `RustPy.AppDir`: `AppRun`, `usr/bin/rustpy`, the desktop file and the icon
(in the AppDir root and under `usr/share`) and `.DirIcon`. It then looks for appimagetool in
`$APPIMAGETOOL` and on `PATH`, and otherwise downloads the continuous build into
`<target dir>/appimagetool` once (`APPIMAGETOOL_URL` changes the source). The result is
`RustPy-<version>-<arch>.AppImage` for the host architecture, about 55 MB. appimagetool
validates the desktop file. Set `APPIMAGETOOL_RUNTIME_FILE` to an explicitly
reviewed runtime for the target architecture; its retained copy at
`usr/lib/rustpy/appimage-runtime` must appear in the runtime manifest. The exact
same file is injected with `--runtime-file`; implicit runtime downloads are disabled. Because it runs with
`APPIMAGE_EXTRACT_AND_RUN=1`, it works in containers and CI without FUSE. Users run the file
directly after `chmod +x`. No libraries are bundled (see above).

### Debian and Ubuntu package

```sh
packaging/linux/build-deb.sh                   # installs cargo-deb if missing, builds, packages
packaging/linux/build-deb.sh --no-build        # package the existing target/release/rustpy
packaging/linux/build-deb.sh --target aarch64-unknown-linux-gnu
sudo apt install ./target/debian/rustpy_0.1.0-1_amd64.deb
```

`build-deb.sh` runs `cargo deb -p rustpy --locked --strip` with any arguments you add, which
can be cargo-deb's own options such as `--deb-revision`, `--dbgsym` or `-o`. Passing
`--no-strip`, `--separate-debug-symbols` or `--dbgsym` replaces the default `--strip`. The
script then prints the control fields and the file list. The `[package.metadata.deb]` section
of `crates/rustpy/Cargo.toml` defines the package:

- files: `/usr/bin/rustpy`, `/usr/share/applications/rustpy.desktop` and
  `/usr/share/icons/hicolor/scalable/apps/rustpy.svg`, plus the generated
  `/usr/share/doc/rustpy/copyright`;
- `Depends: $auto, libgtk-3-0 | libxkbcommon0, libodbc2`; `dpkg-shlibdeps`
  derives linked-library requirements such as `libc6`, `zlib1g` and unixODBC.
  The explicit ODBC runtime dependency must remain even when automatic discovery
  is unavailable; development headers belong on the build machine;
- `Recommends: python3, libxkbcommon-x11-0, libegl1 | libgl1`;
- section `devel`, maintainer `RustPy maintainers <contact@adammudd.co.uk>`.

### Install for the current user from a checkout

```sh
scripts/install.sh                     # cargo build --locked --release -p rustpy, install into ~/.local
scripts/install.sh --prefix /opt/rustpy --no-build
scripts/install.sh --uninstall
```

The script installs three files:

- `<prefix>/bin/rustpy`, with debug info stripped unless you pass `--no-strip`;
- `<prefix>/share/icons/hicolor/scalable/apps/rustpy.svg`;
- `<prefix>/share/applications/rustpy.desktop`, with `Exec` and `TryExec` rewritten to the
  installed binary (quoted per the Desktop Entry spec when the path needs it). The menu entry
  therefore works even when `<prefix>/bin` is not on the desktop session's `PATH`.

It then runs `update-desktop-database` and refreshes an existing GTK icon cache when those
tools are present. If `<prefix>/bin` is missing from `PATH`, it tells you to add it.
`--uninstall` removes the three files and keeps your settings.

The desktop entry registers RustPy for `text/x-python` and `text/x-python3`, so it appears in
*Open With* for `.py` files. It does not register folders, so it can never become the default
file manager; open a folder with `rustpy <dir>`.

## Windows

You need Rust with the MSVC toolchain (Visual Studio Build Tools) and
[Inno Setup 6.3 or newer](https://jrsoftware.org/isdl.php) for the installer. ImageMagick
(`magick`) is optional; with it, the installer gets an icon.

```powershell
powershell -ExecutionPolicy Bypass -File packaging\windows\build.ps1
powershell -ExecutionPolicy Bypass -File packaging\windows\build.ps1 -NoBuild -SkipInstaller
powershell -ExecutionPolicy Bypass -File packaging\windows\build.ps1 -Target aarch64-pc-windows-msvc
```

`build.ps1` builds `rustpy.exe` and renders `rustpy.ico` from the SVG when ImageMagick is
installed. It then writes `target\windows\rustpy-<version>-windows-<arch>-portable.zip`, which
holds `rustpy.exe`, a `README.txt` and `rustpy-portable.cmd`. That launcher points
`RUSTPY_CONFIG_DIR` at a `config` folder next to the executable. Finally, if it finds
`ISCC.exe` (`$env:ISCC`, an `Inno Setup 6` folder under Program Files or
`%LOCALAPPDATA%\Programs`, or `PATH`), it compiles `rustpy.iss` into
`target\windows\rustpy-<version>-windows-<arch>-setup.exe`.

The installer (`rustpy.iss`):

- installs per user into `%LOCALAPPDATA%\Programs\RustPy` without administrator rights. An
  administrator can choose to install for all users (Program Files and HKLM) instead;
- creates the Start Menu shortcut and registers `rustpy.exe` under *App Paths*, so Win+R
  `rustpy` works;
- offers these tasks, all unchecked by default:
  - a desktop shortcut;
  - adding the installation folder to the user's (or, for all users, the system) `PATH`;
  - file associations for `.py`, `.pyw`, `.pyi` and `.ipynb`, each a `RustPy.<ext>` ProgID
    under `<ext>\OpenWithProgids` and `Applications\rustpy.exe\SupportedTypes`. RustPy then
    appears in *Open with* and in *Default apps*, and the current default handler (usually
    the Python launcher) stays the default;
  - Explorer context menu entries: *Open Folder as RustPy Project* on folders and folder
    backgrounds, *Edit with RustPy* on files;
- installs x64 builds on Windows on Arm as well (`x64compatible`). `-Target aarch64-…`
  produces a native arm64 installer;
- removes everything it registered on uninstall, and leaves the settings in
  `%APPDATA%\rustpy` alone.

To override the defaults, run `ISCC.exe` by hand with `/DMyAppVersion=…`, `/DBinDir=…`,
`/DOutDir=…`, `/DArch=x86_64|arm64` and `/DIconFile=…`. A `packaging\windows\rustpy.ico` next to
the script is used automatically. You can also cross-build a portable zip of the Windows
binary from Linux or WSL with `scripts/build-windows.sh` (see [WINDOWS.md](../WINDOWS.md)).

### MSI (WiX)

`build-msi.ps1 -Target x86_64-pc-windows-msvc` (or `aarch64-…`; `-NoBuild` reuses the
release binary) builds `target\windows\rustpy-<version>-windows-<arch>.msi` from
`rustpy.wxs`. It needs WiX 5.0.2 and the matching UI extension, both installed explicitly
(the script never downloads tools):

```powershell
dotnet tool install --global wix --version 5.0.2
wix extension add -g WixToolset.UI.wixext/5.0.2
```

The MSI installs per user into `%LOCALAPPDATA%\Programs\RustPy` without elevation. The
interactive installer shows a feature tree (Welcome, features, Ready; no license page):

| Feature | Contents | Default |
|---|---|---|
| `Core` | `rustpy.exe`, `THIRD-PARTY-NOTICES.txt`, install marker, Start menu shortcut, *Open with* registration (`Applications\rustpy.exe`) | required |
| `FileAssociations` | `.py`, `.pyi`, `.ipynb` → `RustPy.Python`, `RustPy.PythonStub`, `RustPy.Notebook` | on |
| `ContextMenu` | Explorer *Open with RustPy* (files), *Open Folder with RustPy* (folders, backgrounds) | on |
| `AddToPath` | the install folder appended to the user `PATH` | on |

Silent installs pick features with the standard properties, or with convenience properties
that switch an optional feature off (feature level 0) whatever `ADDLOCAL` says:

```powershell
msiexec /i rustpy.msi /qn                                    # everything
msiexec /i rustpy.msi /qn ADDLOCAL=Core                      # only the IDE and its shortcut
msiexec /i rustpy.msi /qn ADDLOCAL=Core,ContextMenu,AddToPath  # no file associations
msiexec /i rustpy.msi /qn ADDLOCAL=ALL REMOVE=FileAssociations # the same
msiexec /i rustpy.msi /qn ASSOCIATE_FILES=0                  # the same
msiexec /i rustpy.msi /qn ASSOCIATE_FILES=0 CONTEXT_MENU=0 ADD_TO_PATH=0
msiexec /x rustpy.msi /qn                                    # uninstall
```

Upgrades keep the features chosen for the previous MSI version (`MajorUpgrade` migrates
feature states); the first upgrade from an MSI without features installs the defaults.
Features can be added or removed later with *Change* in *Installed apps*, or
`msiexec /i rustpy.msi /qn ADDLOCAL=FileAssociations` / `REMOVE=FileAssociations`.

### MSIX (Microsoft Store)

The Store build is a full-trust desktop package (`runFullTrust`, entry point
`Windows.FullTrustApplication`, `Windows.Desktop` 10.0.17763.0 or later) of the unmodified
`rustpy.exe`, for x64 and arm64. The manifest template is
`packaging/windows/msix/AppxManifest.xml.in`; the logos in `msix/Assets` are rendered from
`packaging/linux/rustpy.svg` by `msix/generate-assets.py` (only unqualified sizes; scale
and `targetsize` variants would need a `resources.pri` from makepri.exe).

**Identity placeholders.** `packaging/windows/msix/identity.env` holds placeholder values
until the Partner Center account exists. After reserving the app name, copy them from
Partner Center › the app › *Product management › Product identity*:

| identity.env | Manifest | Partner Center field |
|---|---|---|
| `IDENTITY_NAME` | `Package/Identity/Name` | *Package/Identity/Name* (e.g. `12345RustPy.RustPy`) |
| `PUBLISHER` | `Package/Identity/Publisher` | *Package/Identity/Publisher* (`CN=<GUID>`) |
| `PUBLISHER_DISPLAY_NAME` | `Package/Properties/PublisherDisplayName` | *Package/Properties/PublisherDisplayName* |
| `DISPLAY_NAME` | `Properties/DisplayName`, `VisualElements/@DisplayName` | the reserved app name |

Each can also be set without editing the file through `RUSTPY_MSIX_IDENTITY_NAME`,
`RUSTPY_MSIX_PUBLISHER`, `RUSTPY_MSIX_PUBLISHER_DISPLAY_NAME` and
`RUSTPY_MSIX_DISPLAY_NAME` (for example as CI variables). The package version is the
`Cargo.toml` version as four parts with the last one 0 (`0.1.0` → `0.1.0.0`), which the
Store requires; every submission needs a higher version than the last one.

**Building on Linux/WSL** from the cargo-xwin cross builds:

```sh
cargo xwin build --release --target x86_64-pc-windows-msvc -p rustpy --features native-web-preview
packaging/windows/cross-build-arm64.sh
packaging/windows/build-msix.sh                    # every arch with a release rustpy.exe
packaging/windows/build-msix.sh --arch x64 --sign  # plus a test-signed copy and its .cer
```

It needs Microsoft's `makemsix` from the [msix-packaging](https://github.com/microsoft/msix-packaging)
SDK (`MAKEMSIX=/path/makemsix`, on `PATH`, or `--build-makemsix` to clone and build it into
`target/msix-tools`; the SDK needs CMake 3.29+, the script uses a pip copy when the system
one is older). makemsix writes the block map (SHA-256 per 64 KB block) and
`[Content_Types].xml` and validates the manifest against the AppX schemas. On Linux it can
only write flat bundles, so the `.msixbundle` comes from Windows.

**Building on Windows** with the Windows SDK's makeappx.exe (as `release.yml` does):

```powershell
packaging\windows\build-msix.ps1 -Target x86_64-pc-windows-msvc -NoBuild
packaging\windows\build-msix.ps1 -Target x86_64-pc-windows-msvc,aarch64-pc-windows-msvc -NoBuild -Bundle
packaging\windows\build-msix.ps1 -Target x86_64-pc-windows-msvc -NoBuild -TestSign
```

**Signing.** Store submissions are uploaded **unsigned**: the Store re-signs them with the
publisher certificate that matches `PUBLISHER`. Only sideloaded test packages need a
signature: `--sign` / `-TestSign` create a throw-away self-signed code-signing certificate
whose subject is exactly the manifest `Publisher`, sign a `*-test-signed.msix` copy
(osslsigncode 2.6+ on Linux, `apt install osslsigncode`; signtool on Windows) and export
`rustpy-msix-test.cer`. A tester trusts that certificate in `TrustedPeople` (never a root
store) before `Add-AppxPackage`; `qa/windows/run-msix-acceptance.ps1` does this and removes
it again.

**Submission steps** (once the account exists): reserve the name; fill in
`identity.env`; build the x64 and arm64 packages and the `.msixbundle` (or upload both
`.msix` files); run the Windows App Certification Kit on a Windows PC
(`appcert.exe test -appxpackagepath <bundle> -reportoutputpath report.xml`); create a
submission, upload the bundle under *Packages*, declare the `runFullTrust` restricted
capability and explain why (an IDE that runs interpreters, terminals and tools), fill in the
listing (description, screenshots, real artwork if the logo changes), age rating and
pricing, and submit for certification.

**What differs from the MSI:**

| | MSI | MSIX / Store |
|---|---|---|
| Install location | `%LOCALAPPDATA%\Programs\RustPy` | read-only `C:\Program Files\WindowsApps\<package>`; nothing is written beside `rustpy.exe` |
| `rustpy` in terminals | install folder added to the user `PATH` | app execution alias `%LOCALAPPDATA%\Microsoft\WindowsApps\rustpy.exe` (that folder is on `PATH` by default); no `PATH` edit |
| `.py`, `.pyi`, `.ipynb` | optional feature that registers RustPy ProgIds | declared file type associations: RustPy is offered in *Open with* and *Settings › Apps › Default apps*; Windows never lets a package take a type over |
| Explorer *Open Folder with RustPy* | `ContextMenu` feature (registry verbs) | not available: packaged context menus (`desktop4`/`desktop5` `FileExplorerContextMenus`) require a COM `IExplorerCommand` shell-extension DLL, which RustPy does not ship |
| Updates | RustPy's updater downloads the next `.msi` | the Store updates the package; RustPy detects package identity (`GetCurrentPackageFullName`, falling back to `rustpy-install.kind` = `msix`), never checks or downloads updates itself and says *Updates are managed by the Microsoft Store* in Help › Check for Updates and Settings › Updates |
| Settings, caches, logs | `%APPDATA%\rustpy` | the same paths; Windows may redirect new `%APPDATA%` files of a packaged app into the package's private `LocalCache`, which is removed on uninstall |

`rustpy --verbose --version` prints the detected install kind (`install: msix`,
`package-identity: yes`, `updates: Microsoft Store`).

## macOS

You need the Xcode command line tools for `codesign`, `lipo` and `hdiutil`; `sips`, `iconutil`
and `qlmanage` ship with macOS. `create-dmg` (`brew install create-dmg`) and `rsvg-convert`
(`brew install librsvg`) are optional. To distribute beyond your own machines, you also need
an Apple Developer ID.

```sh
packaging/macos/build-app.sh                    # cargo build, RustPy.app, RustPy-<version>-arm64.dmg
packaging/macos/build-app.sh --universal        # arm64 + x86_64 merged with lipo
packaging/macos/build-app.sh --no-build --no-dmg
CODESIGN_IDENTITY="Developer ID Application: Example (TEAMID)" NOTARY_PROFILE=rustpy \
    packaging/macos/build-app.sh --universal
```

`target/macos/RustPy.app` contains:

- `Contents/MacOS/rustpy` and `Contents/PkgInfo`;
- `Contents/Info.plist`: bundle id `dev.rustpy.RustPy` (or `$BUNDLE_ID`), the version from
  `Cargo.toml`, the Developer Tools category, Retina support, automatic graphics switching
  and a minimum of macOS 11;
- `Contents/Resources/rustpy.icns`. The script renders it from the SVG (`rsvg-convert`,
  `magick` or Quick Look, then `sips` and `iconutil`) or copies it from `--icon FILE.icns`.
  When neither is possible, the SVG is stored as a placeholder and Finder shows the generic
  icon.

The app is signed with `CODESIGN_IDENTITY` (hardened runtime, timestamp) or, without it, ad
hoc, which Apple Silicon requires. The disk image comes from `create-dmg` if installed (a
Finder window with an *Applications* drop link; the Finder scripting is skipped when `CI` is
set, as on GitHub Actions). Otherwise it comes from `hdiutil create -format UDZO`, with an
`Applications` symlink. With `NOTARY_PROFILE` (a profile saved by
`xcrun notarytool store-credentials`), the disk image is also notarized and stapled.

The app itself has two macOS limitations:

- Apps started from Finder or the Dock inherit launchd's minimal `PATH`, so a Homebrew Python
  in `/opt/homebrew/bin` is not discovered. Select it in *Settings › Project › Python
  Interpreter*, or start `RustPy.app/Contents/MacOS/rustpy` from a terminal.
- winit does not deliver Finder "open document" events (an app has to install its own
  `NSApplicationDelegate` for `application:openURLs:`), so the bundle declares no document
  types and offers no *Open With* entry yet.

## Icons

`packaging/linux/rustpy.svg` is the single source of every icon. It uses only filled shapes in
solid colours, so librsvg, resvg, browsers, Quick Look and ImageMagick's built-in renderer
all draw it the same way, and it stays legible at 16 px. `build.ps1` and `build-app.sh`
render the `.ico` and `.icns` at build time. To make them by hand:

```sh
magick -background none -density 384 packaging/linux/rustpy.svg \
    -define icon:auto-resize=256,128,64,48,32,16 packaging/windows/rustpy.ico
rsvg-convert -w 1024 -h 1024 -o icon-1024.png packaging/linux/rustpy.svg   # then sips + iconutil
```

`rustpy.exe` has no embedded icon resource yet (that would need a `build.rs`), so Explorer
shows the generic program icon for the executable itself. The installer's shortcuts use
`rustpy.ico`.

## Release checklist

The complete checklist is in [docs/release.md](../docs/release.md); the packaging steps are:

1. Bump `version` in the root `Cargo.toml`. Run `cargo clippy --workspace --all-targets`,
   `cargo test --workspace` and the headless smoke test (`scripts/smoke.sh`).
2. `.github/workflows/release.yml` is manual (`workflow_dispatch`). Enable its Windows
   and publish job guards for an approved release, dispatch once, then restore
   both guards to `false`. Other platform and Microsoft Store jobs stay disabled.
   It builds x64 and ARM64 portable ZIPs, setup executables and MSI installers, checks
   expected outputs, then generates and independently verifies a combined `SHA256SUMS`.
3. After both Windows builds succeed, the publisher creates `vX.Y.Z` from the workspace
   version at the workflow run's exact source revision. It uploads packages and checksums
   to a draft GitHub Release, then publishes it. A failed build does not publish a release;
   publication failures may leave a draft for inspection. Existing releases are not replaced.
4. Check them. Inspect the `.deb` with `dpkg-deb --info` and `--contents`. Install the setup
   `.exe` in a Windows VM. On macOS, mount the `.dmg` and run `spctl -a -vv RustPy.app`.

## Bundled resource notices

Every artifact includes `Native-Licenses.zip`, containing exact covered Cargo
sources and retained native component source/license evidence. `packaging/stage-native-licenses.py`
creates it offline from the Cargo cache (or vendored Cargo sources). Any bundled
release executable requires a reviewed `RUSTPY_NATIVE_RUNTIME_MANIFEST` binding
its SHA256 and explicitly declaring system/toolchain static native components
(or an empty list when none are present). Bundled runtime libraries require exact
source archives, license texts and replacement instructions; unknown runtimes
stop packaging. Regenerate the binary hash after staging/stripping. Make that manifest and its referenced files available inside
Flatpak/Snap build environments too. Host libraries are supplied by the system.

Every artifact includes the project Apache-2.0 `LICENSE` beside the third-party
notice bundle. Packaging recipes reject missing or stale retained notices before
building or staging artifacts; Python 3 is required (the `python` command on Windows).

macOS records `binary_stage: pre-sign`: the linkage manifest hashes the staged
executable before code signing. Signing changes its bytes and seals the source
archive as a bundle resource, so embedding a hash of the final signed executable
would create a circular resource seal. Verify the final signed artifact separately
with the release checksums; the embedded manifest describes its pre-sign code.

`THIRD-PARTY-NOTICES.txt` preserves licenses and notices for every registry
package in the application, QA and fuzz lockfiles, embedded resources and vendored
GUI sources. Regenerate Cargo evidence with `python3 scripts/generate-cargo-notices.py`
after dependency changes, then update the aggregate with
`python3 packaging/update-notices.py`; verify source-text drift with
`python3 packaging/update-notices.py --check` before packaging. Missing source
texts fail rather than producing an incomplete bundle. This is separate from
the Cargo dependency license/advisory gate.

The archive/Windows installers ship the file beside the executable; macOS
ships it in `RustPy.app/Contents/Resources`; Linux native packages, AppImage,
Snap, Flatpak and user-prefix installs ship it in `share/doc/rustpy` under their
respective install roots. Keep it when redistributing the binary. The source-only
Black test corpus retains its own license/provenance beside the fixtures.
