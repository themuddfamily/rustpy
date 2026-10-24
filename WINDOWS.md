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
