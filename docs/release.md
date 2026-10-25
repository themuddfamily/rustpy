# Release checklist

Use this list when preparing a RustPy release. Dispatch `.github/workflows/release.yml`
by hand with `publish=false` for a dry run, then use `publish=true` only after its artifacts
have been checked. The workflow builds Linux, Windows and macOS packages but uploads and
publishes Windows assets only. Publication waits for both Windows builds; Linux and macOS
jobs do not gate it. Flatpak/Snap builds require the separate `sandbox_packages` input;
Microsoft Store jobs remain disabled.

1. **Pick the revision.** Start from a clean checkout of the commit to release. Update
   `version` in the root `Cargo.toml` and move the `[Unreleased]` entries of
   [CHANGELOG.md](../CHANGELOG.md) under the new version and date.
2. **Run the quality gates** in [quality.md](quality.md) on that revision: fmt, Clippy, unit
   and doc tests, the `qa/` suite, cargo-deny and the headless smoke test. Record the
   revision, toolchain, platform, test counts, ignored tests and any remaining failures.
   Older screenshots or results from another revision do not count.
3. **Review dependencies and notices.** Review `Cargo.lock` changes, advisories and licences,
   vendored code, fonts, grammars and corpus provenance. Make sure no secrets, tokens, trust
   stores or machine-specific paths are in the tree or the artifacts. Then run:
   ```sh
   python3 scripts/generate-cargo-notices.py --check
   python3 packaging/update-notices.py --check
   ```
   After dependency updates, regenerate the [Cargo inventory](dependency-licenses.csv) and
   the notices with `scripts/audit-dependency-licenses.py`, `scripts/generate-cargo-notices.py`
   and `packaging/update-notices.py`, in that order, and review the pinned upstream overrides
   for new versions.
4. **Prepare native licence evidence** for the actual staged binary and toolchain as described
   in [native-license-evidence.md](../packaging/native-license-evidence.md), including system
   libraries linked statically, and set `RUSTPY_NATIVE_RUNTIME_MANIFEST`. AppImage builds also
   need `APPIMAGETOOL_RUNTIME_FILE` and the evidence for that runtime. Offline vendored builds
   need the original `.crate` archives in the Cargo cache or `RUSTPY_NATIVE_CRATE_CACHE`.
   Packaging stops when notices, covered sources or linkage declarations are missing.
5. **Build the artifacts** with the [packaging recipes](../packaging/README.md): Linux packages,
   Windows Inno Setup and WiX 5.0.2 (with `WixToolset.UI.wixext/5.0.2`), the macOS bundle and
   DMG. `native-web-preview` needs each platform's browser SDK. A cross-build does not replace
   a native test. Check that every artifact contains `LICENSE`, `THIRD-PARTY-NOTICES.txt` and
   `Native-Licenses.zip`, and that the archive holds the covered sources and build
   instructions. On macOS the linkage manifest describes the executable before signing
   (`binary_stage: pre-sign`); verify the signed artifact with the release checksums.
6. **Test installation** on each supported OS and architecture: fresh install, upgrade,
   launcher and file associations, uninstall (settings are kept), and a portable
   configuration directory. Test the update check: manifest, hash and signature verification,
   rejection of corrupt or partial downloads, and cancellation. Use a separate test profile,
   not your own settings.
7. **Checksums and signing.** Generate `SHA256SUMS` with `scripts/release-checksums.py`,
   verify the candidate artifacts against it independently, then sign and notarise.
8. **Release notes.** Generate them from the changelog (`scripts/release-notes.sh`) and make
   sure they state the verified platforms, optional dependencies and known limitations
   honestly (see [review-backlog.md](review-backlog.md)).
9. **Publish.** Tag `vX.Y.Z` (pre-releases `vX.Y.Z-eap.N` or `vX.Y.Z-beta.N`) on the released
   revision, upload the artifacts and checksums to a draft GitHub release, review it, and
   publish. For Windows workflow publication, `scripts/publish-release.sh` checks that
   `SHA256SUMS` covers exactly the six expected x64/ARM64 ZIP, setup EXE and MSI packages
   and verifies their hashes. It creates a draft or resumes an unpublished one, updates
   its target revision and notes, uploads all assets and checksums, then publishes.
   Let the workflow create its tag; existing tags and published releases are refused.
   Keep local artifacts and evidence until the release is out.
