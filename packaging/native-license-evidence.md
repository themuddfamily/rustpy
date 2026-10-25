Native distribution evidence
============================

`collect-native-licenses.py --output EMPTY_DIRECTORY --runtime-root STAGING_ROOT
--binary ACTUAL_PACKAGED_EXECUTABLE --runtime-manifest JSON`
exports exact locked native crate archives and their nested license texts. The
archive checksum must match Cargo.lock. `--cargo-vendor DIRECTORY` instead
accepts an intact Cargo vendor tree, verifies every file against the original
checksum-verified .crate archive, and exports those source trees. Original .crate
archives must remain available in Cargo cache or RUSTPY_NATIVE_CRATE_CACHE
(recursively searched). Offline Flatpak builds must provision these originals
when preparing the vendor input. A self-asserted .cargo-checksum.json alone is
insufficient authentication. Publish the
result with the binary; retain the complete sources, not only the notices.

`--runtime-manifest JSON` (or RUSTPY_NATIVE_RUNTIME_MANIFEST) is always required.
It must include `binary_sha256`, verified against `--binary`, and an explicit
`system_static_components` array. An empty array declares that no native code
from system/toolchain packages is statically linked; otherwise each component
requires name, exact version, license_files, source_archives, build_instructions
and replacement_instructions using the same evidence schema as runtime entries
(without `files`). Native code already retained from authenticated locked crate
archives need not be duplicated. Record actual linkage from build/linker evidence;
do not infer this declaration from an empty directory or absence of DLLs.
Retain evidence for compiler runtimes statically included in the executable as
well as statically linked OpenSSL or other system/toolchain libraries. Hash the
final packaged binary after stripping/signing or other transformations.
The default --binary-stage packaged records that stage. macOS may explicitly
use --binary-stage pre-sign when bundling evidence before signing; the retained
manifest records that the binary hash precedes signing, and the final signed
artifact requires its separate release checksum. `--binary`
can point outside STAGING_ROOT, allowing a documentation-only packaging stage.
The declaration is producer evidence, not automatic detection of static code.

Microsoft redistributable object-code evidence
---------------------------------------------

Windows MSVC startup code remains statically linked even when the UCRT is dynamic.
Do not declare `system_static_components: []` because no CRT DLL is bundled.
The collector also accepts the explicit `evidence_kind: microsoft-redistributable`
category for these exact Microsoft runtime families: `msvc-crt` (Microsoft Visual
C++ CRT), `windows-sdk` (Microsoft Windows SDK) and `webview2-loader` (Microsoft
WebView2 Loader). The primary executable must be Windows PE, the vendor must be
Microsoft, and the component needs an exact numeric version. This category
records the applicable vendor-supplied object-code redistribution terms instead
of inventing unavailable source archives. It is not a generic source exemption.
Existing open-source components still require their exact sources, licences,
build instructions and replacement instructions.

Each vendor entry requires SHA256 maps named `vendor_terms`,
`redistribution_evidence` and `linkage_evidence`; `license_files` must reference
the retained vendor terms. `linkage_records` identifies the final map, trace and
build record, all retained with hashes. The build record binds the executable,
compiler commit and original linker outputs. `input_inventory` identifies a
retained JSON file with the same binary SHA256, runtime family and nonempty
logical library names/hashes. `provenance_urls` must point to official HTTPS
Microsoft/NuGet resources, and `review_basis` records the producer's basis for
redistribution. A completed evidence bundle is not a legal certification.

`scripts/prepare-windows-native-evidence.py --binary EXE --link-map MAP
--link-trace TRACE --link-record RECORD --toolchain-record TOOLCHAIN --output DIR`
runs on the native Windows runner after the final link and writes
`DIR/runtime-manifest.json`. It validates recorded binary/map/trace hashes,
records actual linked inputs and installed Rust/Visual Studio/SDK versions, and
retains authenticated Rust/native Cargo sources (including ring and blake3).
Rust's official full source archive is additionally retained when the selected
compiler-builtins archive contains native C/assembly objects not supplied by
the rust-src component. Unknown native inputs stop production. The producer
supports the reviewed Visual Studio 2022 (product major 17) and Visual Studio
2026 (product major 18) terms profiles, selected from the actual installed product
version. It downloads that product year's distinct build-tool, product/runtime
EULAs and redistribution list; an unknown major or an unreviewed Preview channel
is rejected. It rejects VC/debug CRT DLL dependencies for these
self-contained installers; use the recorded `+crt-static` target build.
Run `--preflight --toolchain-record TOOLCHAIN --output DIR` before compiling to
validate installed SDK terms and fetch authenticated Rust sources/vendor terms.
Preflight writes no runtime manifest; final binary and linker binding remain
mandatory. Full production reuses checksum-verified preflight sources.
Temporary rustc raw-dylib import archives may disappear after linking. The
recorder must preserve their bytes and hashes before returning to rustc. The
producer accepts only recorded raw-dylib archives containing COFF import
descriptors/short import objects, rejects native function-code substitutes and
retains the generated archives as provenance, not as invented source archives.
Ordinary components may attach additional checksum-bound `provenance_files`;
their actual `source_archives` requirement remains mandatory.
SDK terms are retained from its dedicated installed `Licenses` directory (or an
explicit `--windows-sdk-terms` path). Missing SDK terms stop the producer; Visual
Studio's redistribution list is not substituted for a Windows SDK licence.
Downloaded Microsoft terms must be actual DOCX licence documents, not HTML error
pages. The retained documents describe the producer's review basis; the tool does
not claim that a generic REDIST list individually names every static CRT object.

WebView2's native loader is covered by its matching official SDK terms, not the
Rust wrapper's MIT declaration. The producer derives the SDK version from the
locked wrapper's upstream generator revision, compares the official NuGet loader
with the authenticated crate payload and the actual linked wrapper archive's
object members, then retains the SDK's licence, notices, package metadata and
payload hashes. The current SDK uses Microsoft BSD-style binary/source
redistribution terms. Its notices remain part of the distribution evidence;
the producer does not manufacture a nonexistent REDIST file or assert that the
SDK source is included. Vendor object archives are hashed, not redistributed
as standalone `.lib` files. Reviewed source/replacement obligations for any
open-source runtime components remain unchanged.

Primary references:
https://learn.microsoft.com/en-us/cpp/c-runtime-library/crt-library-features
https://www.microsoft.com/licensing/guidance/Visual-Studio
https://learn.microsoft.com/en-us/visualstudio/releases/2022/redistribution
https://learn.microsoft.com/en-us/visualstudio/releases/2026/redistribution
https://visualstudio.microsoft.com/license-terms/

The manifest's `components` array inventories redistributed shared libraries. The script's module documentation
contains the schema. Library paths are relative to STAGING_ROOT; all evidence
paths are relative to the JSON. File and source archive SHA256 values bind the
evidence to the actual artifacts. Include distribution patches and build scripts
in corresponding source, with exact package version/revision in `version`.
License files must be the actual package copyright/license files, including all
applicable exceptions, third-party notices and attribution. The collector
requires source, build instructions and replacement instructions for every
ordinary component, conservatively including permissive libraries; only the
explicit Microsoft vendor category above uses its retained redistribution evidence.
It rejects any
uncovered ELF/PE/Mach-O files (including executables and extensionless AppImage
runtime), and .dll/.so/.dylib paths even when format detection is inconclusive.
Only the exact primary --binary path and collector output are excluded. When
staging a copy of the executable, pass the staged copy as --binary. It validates coverage and integrity; a producer still
must verify that supplied evidence actually corresponds to the shipped library.

For Debian/Ubuntu builds, obtain the library owner with `dpkg-query -S`, retain
`/usr/share/doc/PACKAGE/copyright`, and obtain the exact source package/version
reported by `dpkg-query -W -f='${source:Package} ${source:Version}' PACKAGE`
using `apt-get source --download-only SOURCE=VERSION`. Retain the .dsc, original
source archives, Debian patch archives and build instructions. A newer upstream
tarball does not substitute for distribution-patched corresponding source.
Other distributions need equivalent package-owner and exact-source evidence.

MinGW GCC runtime DLLs and winpthreads are separate components. Preserve the
actual toolchain package licenses. GCC libstdc++ is GPLv3 with Runtime Library
Exception 3.1; the exception concerns eligible compilation and does not waive
all obligations on separately redistributed libraries. winpthreads' upstream
COPYING includes both the mingw-w64 MIT terms and Lockless BSD-style terms; do
not label it only as a GCC exception or omit either notice. Toolchain revisions
can differ; the exact package evidence is authoritative.

Flatpak's unixODBC 2.3.12 must retain its COPYING/LGPL and relevant file headers,
exact source archive, patches and actual module build instructions. Keep the
library dynamically replaceable and provide concrete instructions for exporting
an installed app, rebuilding the library with the recorded options, replacing
/app/lib/libodbc.so*, and running the replacement (including any sandbox override
needed). Permit reverse engineering for debugging modifications to the LGPL
library. Do not prohibit that activity through an installer or EULA. LGPL source
and replacement requirements apply even when the main application is permissive.

GTK/WebKitGTK and their transitive AppImage libraries are not cleared by a Rust
wrapper crate's license. Collect each actually copied library's package-specific
license and matching complete corresponding source/build material. Preserve a
usable replacement mechanism and instructions for LGPL components, for example
extracting the AppImage, rebuilding a library and replacing it before AppRun.
Do not bundle those libraries until their evidence passes the gate. System
libraries required from the host but not copied are outside this runtime bundle
inventory; do not represent their absence as license clearance of a static copy.

OpenSSL linked from a system/toolchain package also needs package-specific
notice evidence when its code is redistributed, including when statically
linked. OpenSSL 1.x and 3.x have different terms. The native crate collector
retains openssl-src only when present in Cargo.lock; it does not manufacture
OpenSSL provenance from openssl-sys's wrapper license. A static library producer
must supply exact source/license/build evidence for system-sourced native
code through system_static_components; the distribution gate refuses to proceed
without the explicit binary-bound declaration.

Primary terms consulted:
https://github.com/libgit2/libgit2/blob/main/COPYING
https://gcc.gnu.org/onlinedocs/libstdc++/manual/license.html
https://raw.githubusercontent.com/mingw-w64/mingw-w64/master/mingw-w64-libraries/winpthreads/COPYING
https://www.gnu.org/licenses/old-licenses/lgpl-2.1.html
https://www.unixodbc.org/unixODBC.html

The retained libgit2 COPYING is from the locked 1.9.7 upstream payload, including
its GPLv2 linking exception. Do not substitute upstream main for exact release
terms. Sources exported from .crate archives retain embedded component source
and notices (including zlib, llhttp and WinHTTP compatibility sources). SQLite's
public-domain dedication is retained from the bundled amalgamation header.
