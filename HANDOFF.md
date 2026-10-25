# RustPy maintainer status

This page is for whoever picks up RustPy maintenance: what is finished, what is partial, what
still has to be verified before a first public release, and the known open issues. Feature
detail lives in [roadmap.md](roadmap.md); user-facing issues are listed in
[docs/review-backlog.md](docs/review-backlog.md).

## Summary

- Version 1.0.0, not yet released. [CHANGELOG.md](CHANGELOG.md) has a dated `1.0.0` section
  prepared for the first public release; the GitHub release remains a draft until packaging
  and acceptance checks are complete.
- Roadmap: 291 feature items, 286 done (`[x]`), 5 partial (`[~]`), none untouched. All five
  partial items are implemented; what is missing is verification on macOS (and, for one item,
  on Windows on Arm and as a Snap).
- Linux is the primary development platform and the best-tested one. Windows 11 (x64) has been
  checked for interpreters, browsers, the web preview and the installers. macOS builds have not
  been run on a Mac.

## Complete

Everything in the roadmap marked `[x]`: the editor, Python code insight (parser, type
inference, inspections, intentions, completion, navigation, refactorings, formatter), run and
debug, the test runners, coverage and profiler, Git and the other VCS integrations, the
terminal and Python Console, packages and interpreters, Jupyter notebooks and the scientific
viewers, the HTTP client, database tools, Docker, Kubernetes and remote interpreters,
deployment, frameworks (Django, Flask, FastAPI, Jinja2 and others), plugins, localisation, the
language server and the AI Assistant.

The last full local run of the gates (2026-10-17) was clean: `cargo fmt --check`, workspace
Clippy with `-D warnings`, `cargo deny`, the `qa/` crate tests, the Python script tests and the
full workspace test suite (6,643 tests passed). The release build passed the performance
harness on 2026-10-18 (10k- and 100k-file projects, million-line analysis; software rendering).

## Partial

| Roadmap item | What is missing |
|---|---|
| §6 Interpreter discovery | macOS framework and Homebrew interpreter paths not verified on a Mac |
| §6 Add Interpreter dialog | macOS flows not verified |
| §6 Web browsers and preview | Safari and the WKWebView built-in preview not verified |
| §8 Cross-platform builds | Snap package, macOS bundle (universal build, signing, notarisation) and running the Windows arm64 build on Arm hardware |
| §8 Accessibility | NVDA (Windows) and VoiceOver (macOS); Linux was checked with Orca |

## Verification outstanding before a release

The 1.0.0 preparation checks on 2026-10-25 used Rust 1.99.1 on Linux x86_64, two CPUs,
nice 10, two Cargo jobs, two Rayon workers and at most two test threads. Formatting,
Clippy (`-D warnings`), the application build, QA (6 passed, 1 ignored), Python checks
(30 passed), distribution contracts (3 passed), dependency checks, notices and the covered
Cargo source bundle passed. Dark and Light headless smoke screenshots were captured and
reviewed using isolated settings.

The clean-environment workspace run had **6,701 passed, 2 failed and 164 ignored**.
The cancellation-latency test exceeded its 250 ms bound (779 ms), and the cancelled-clone
fixture observed cleanup before transport retirement. Both passed once in isolation with
one test thread (cancellation 126 ms); this does not establish a clean full-suite gate.
An earlier console failure was caused by the harness setting `PYTHONDONTWRITEBYTECODE`;
the console test passed after that flag was removed. The UI snapshot checks were required.

The [packaging dry run](https://github.com/themuddfamily/rustpy/actions/runs/40184646017)
on revision `05d378ffb31b9d28e729340cd5ca086f6c9d5b61` built Linux x64 successfully, then
failed native licence staging because the binary-bound runtime manifest was missing.
The remaining jobs were cancelled. No installers, public tag or release were published;
the 1.0.0 release remains a draft. Native evidence and installation acceptance are still
required before publication.

The source has changed since the last full run (Windows packaging, licence tooling, and the
public-release clean-up that renamed the colour schemes and replaced the bundled font). Before
tagging a release:

1. Run every gate in [docs/quality.md](docs/quality.md) on the release revision: fmt, Clippy,
   unit and doc tests, the `qa/` suite, cargo-deny and the Python script tests.
2. Review the UI snapshot goldens after the font change (editor font metrics changed), then run
   the snapshot tests with `RUSTPY_REQUIRE_SNAPSHOTS=1`.
3. Regenerate and check the licence notices (`packaging/update-notices.py --check`,
   `scripts/generate-cargo-notices.py --check`).
4. Run the headless smoke test (`scripts/smoke.sh`) and look at the screenshots in both the
   Dark and Light schemes.
5. Corpus, CPython oracle and fuzz gates need provisioned corpora and a nightly toolchain; they
   have not yet been run in a recorded, reproducible way.
6. Follow [docs/release.md](docs/release.md) for packaging, installation tests and signing.

## Continuous integration

`.github/workflows/ci.yml` runs fmt, Clippy, tests, the desktop build and cargo-deny on
Linux, Windows and macOS for every push to `main` and every pull request; all fourteen jobs
are green as of 2026-10-24. Tests that depend on Unix tools, Unix fixture paths or behaviour
not yet understood on the hosted Windows and macOS runners are gated or ignored there, each
with its reason in the source and an entry in the known-issues list.
`.github/workflows/release.yml` is dispatched by hand: it builds the Linux, Windows and macOS
packages, and publishing a GitHub release or building Flatpak/Snap packages are opt-in
inputs. The Microsoft Store (MSIX) steps stay disabled until the package identity is real.

## Known open issues and limitations

- **macOS** is untested on real hardware: app bundle, signing, notarisation, Finder
  integration, interpreter discovery and the WKWebView preview.
- **Windows terminal**: stopping a ConPTY-backed terminal session was the last Windows terminal
  issue found; the fix is integrated but has not been re-run on Windows. PowerShell, cmd and
  WSL terminal profiles, and zsh, fish and pwsh shells, have not been checked end to end.
- **Windows on Arm**: the arm64 build cross-compiles but has not been run on Arm hardware.
- **Snap**: the Snap package is defined but has not been built (needs snapd).
- **Microsoft Store**: the MSIX package is sideload-tested; a Store submission needs a Partner
  Center identity.
- **Frameworks**: Pyramid, web2py, Mako and Chameleon are covered by automated tests only;
  Google App Engine deployment is not checked.
- **Notebooks**: Docker-hosted kernels are not a verified transport.
- **Databases**: MongoDB SRV connection strings, and SSH tunnels combined with non-direct
  proxies, are not supported.
- **Collaboration**: there is no hosted relay and no voice; sessions use direct, authenticated
  connections.
- **Rendering**: only software rendering (llvmpipe) has been measured; hardware GPU rendering
  has not been benchmarked.
- **Desktop integration**: real desktop-notification daemons, native macOS window tab merging
  and Wayland-specific behaviour have not been checked.

## Packaging placeholders to fill in

- The MSIX manifest uses a placeholder package identity
  (`packaging/windows/msix/identity.env`) until a Partner Center product exists.
- Signing and notarisation credentials are not part of the repository.

## Where things are

- [README.md](README.md): overview, build instructions, crate map.
- [docs/architecture.md](docs/architecture.md): crate boundaries and data flow.
- [docs/contributing.md](docs/contributing.md): conventions, checks, pull requests.
- [docs/quality.md](docs/quality.md): every quality gate and how to run it.
- [docs/release.md](docs/release.md): release checklist.
- [packaging/README.md](packaging/README.md): installers and licence notices.
- [AGENTS.md](AGENTS.md): rules for automated contributors.
