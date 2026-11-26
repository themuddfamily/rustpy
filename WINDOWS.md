# Build RustPy for Windows from WSL

Build a native 64-bit Windows executable entirely inside WSL or Linux using
Rust and MinGW. The build uses Linux tools and does not require Visual Studio,
a Windows SDK, or a Windows build runner. The resulting executable runs on
Windows without WSL or Rust installed.

## Install the build tools

Run these commands inside an Ubuntu or Debian WSL terminal. Rust and Cargo
must already be installed through [rustup](https://rustup.rs/).

```bash
sudo apt-get update
sudo apt-get install -y mingw-w64 zip
rustup target add x86_64-pc-windows-gnu
```

MinGW provides a compiler, linker, and Windows libraries that run on Linux.
The Rust target supplies the Windows standard library. See the official
[Rust Windows GNU target documentation](https://doc.rust-lang.org/rustc/platform-support/windows-gnu.html)
and [MinGW setup instructions](https://www.mingw-w64.org/getting-started/debian/).

## Build and package the executable

From this repository in WSL:

```bash
cd path/to/rustpy
./scripts/build-windows.sh
```

The first build
takes longer while Cargo downloads and compiles dependencies. Later builds
reuse the compiled dependencies.

The script builds the `rustpy` package in release mode for
`x86_64-pc-windows-gnu`, removes debug information from the packaged copy, and
creates these outputs:

| Path | Contents |
| --- | --- |
| `target/windows-portable/rustpy.exe` | Windows executable ready to copy |
| `target/rustpy-windows-x86_64.zip` | Portable distribution containing the executable and any required MinGW runtime DLLs |
| `target/x86_64-pc-windows-gnu/release/rustpy.exe` | Original release executable with debug information |

If `CARGO_TARGET_DIR` is set, the outputs go under that directory instead of
`target`. The script reports the output paths and imported DLLs when it finishes.
Run `./scripts/build-windows.sh --help` for setup and usage information.

## Fast test builds

For trying a change on Windows by hand, `--fast` builds a test executable in
seconds to minutes instead of a full optimized release:

```bash
export CARGO_TARGET_DIR=$HOME/rustpy-target-win   # keep it: it is the warm cache
./scripts/build-windows.sh --fast --out /mnt/c/Users/YOUR_NAME/Downloads
```

The script prints the executable and ZIP paths both as Linux paths and as
`\\wsl.localhost\...` paths that Windows Explorer opens directly; `--out DIR`
also copies the ZIP into `DIR`. `--no-zip` skips the ZIP, for running
`windows-test\rustpy.exe` straight from the `\\wsl.localhost` path.

`--fast` uses its own `wintest` Cargo profile, so it never replaces the release
output. Its outputs are `windows-test/rustpy.exe` and
`rustpy-windows-x86_64-test.zip`. The profile turns off LTO, uses many codegen
units, omits debug information and compiles workspace crates incrementally.
Dependencies are built at full optimization once; workspace crates use
opt-level 2 and the large `rustpy-ui` crate (with the `rustpy-ui-*` crates split out
of it) opt-level 0, because at a higher level an edit in it spends minutes in code
generation. Change them with
`--opt-level N` and `--ui-opt-level N`.

### Toolchains

By default (`--toolchain auto`) the fast build uses the MSVC target when it is
available and MinGW otherwise; the script prints which one it used.

- `--toolchain msvc` builds `x86_64-pc-windows-msvc`, the target of official
  releases, with [cargo-xwin](https://github.com/rust-cross/cargo-xwin)'s
  `clang-cl` and `lld-link`, the static C runtime (`+crt-static`) and the
  `native-web-preview` feature (WebView2) that releases ship. It needs
  `cargo install cargo-xwin`, the `clang`, `lld` and `llvm` packages,
  `rustup target add x86_64-pc-windows-msvc`, and cargo-xwin's copy of the
  Microsoft CRT and Windows SDK. Downloading that copy means accepting
  Microsoft's licence, so the script never does it: run a `cargo xwin build`
  once yourself to download it.
- `--toolchain gnu` builds `x86_64-pc-windows-gnu` with MinGW, linked with LLD
  when `ld.lld` is installed (`--linker bfd` selects the slower MinGW `ld`).
  It needs only the packages of the default mode, and builds without
  `native-web-preview` unless `--features native-web-preview` is given.

### Build times

The first fast build compiles every dependency: 5–11 minutes on a 32-core WSL2
machine limited to 10 cores, longer when other builds share it. Later builds
recompile only what changed and take well under a minute, ZIP included:

| Change | MSVC | MinGW |
| --- | --- | --- |
| None | 5–9 s | 5–8 s |
| A line in `rustpy-ui` | 20–35 s | 25–30 s |
| A line in a leaf crate such as `rustpy-emmet` | 15–20 s | 20–25 s |
| A line in `rustpy-semantic` | 25–45 s | not measured |
| A line in `rustpy-core` (38 crates) | 30–45 s | about 35 s |

These were measured while other builds shared the machine; on a busy machine
an edit in `rustpy-ui` at `--ui-opt-level 1` took up to four minutes.

Switching options such as `--opt-level` or the toolchain rebuilds the affected
crates once; each combination keeps its own cached artifacts.

### What a test build does not prove

A fast test build is not a release. It is less optimized, it has no installer,
MSI or MSIX package, no native license bundle and no signature, and it is
built on Linux with `clang-cl`/`lld-link` (MSVC target) or MinGW instead of the
Visual Studio toolchain of the release runners. Use it to try a change, not to
distribute it, and check release-only behaviour (installers, signing, startup
performance) with a release build.

## Run the program on Windows

Copy the ZIP to a Windows folder, extract it, and double-click `rustpy.exe`.
Windows release builds open the GUI without an accompanying console window.
Debug builds retain the console for development. Release logs are written to
`log/rustpy.log` under the configuration directory.
For example, copy it into your Windows Downloads folder from WSL:

```bash
cp target/rustpy-windows-x86_64.zip /mnt/c/Users/YOUR_NAME/Downloads/
```

Replace `YOUR_NAME` with your Windows account folder name. Keep any DLLs in
the ZIP beside the executable. Windows system DLLs are supplied by Windows
and are not included in the package.

The IDE uses Windows paths and Windows interpreters when running on Windows.
Install or configure a Windows Python interpreter to execute Python projects;
the ZIP does not bundle Python. Features that invoke external tools, such as
Git, need those tools available on Windows as well.

The executable can run without an installer, but settings and logs normally
live under the user's configuration directory. Set `RUSTPY_CONFIG_DIR` to a
writable directory before launching if you need a different settings location.
The ZIP is a portable executable package; it does not create Start menu
shortcuts or an uninstaller.

## Automated release builds

The `.github/workflows/release.yml` workflow builds the Windows GNU target
on an Ubuntu runner using the same script. Run **Actions → Release builds →
Run workflow** to build a ZIP artifact. Pushing a `v*` tag also attaches the
ZIP to a GitHub release. Commit and push the build script and workflow changes
before running this workflow on GitHub.

## Check the build

These checks run entirely inside WSL:

```bash
file target/windows-portable/rustpy.exe
unzip -t target/rustpy-windows-x86_64.zip
x86_64-w64-mingw32-objdump -p target/windows-portable/rustpy.exe |
    awk '/DLL Name:/ {print $3}'
```

`file` should report a `PE32+` executable for Windows on x86-64. These checks
verify the file format, archive integrity, and DLL imports. They do not test
the Windows GUI, Python execution, or other runtime behavior. Test those on
Windows before distributing a release; Wine can also provide an initial
runtime check inside Linux.

If the script reports a missing tool or Rust target, repeat the setup commands
above. A normal `cargo build --release` inside WSL builds the Linux version;
use the script to produce the Windows package.
