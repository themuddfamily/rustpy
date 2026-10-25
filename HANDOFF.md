# RustPy maintainer status

This page is for whoever picks up RustPy maintenance: what is finished, what is partial, what
still has to be verified for future releases, and the known open issues. Feature
detail lives in [roadmap.md](roadmap.md); user-facing issues are listed in
[docs/review-backlog.md](docs/review-backlog.md).

## Summary

- Version 1.0.0. [CHANGELOG.md](CHANGELOG.md) has a dated `1.0.0` section for the first public
  release. [RustPy v1.0.0](https://github.com/themuddfamily/rustpy/releases/tag/v1.0.0)
  is published with Windows x64 and ARM64 setup, MSI and portable packages plus checksums.
- Roadmap: 291 feature items, 286 done (`[x]`), 5 partial (`[~]`), none untouched. All five
  partial items are implemented; what is missing is verification on macOS (and, for one item,
  on Windows on Arm and as a Snap).
- Linux is the primary development platform and the best-tested one. Windows 11 (x64) has been
  checked for interpreters, browsers and the web preview. Native x64 and ARM64 installer
  automation passed. macOS builds and automated tests passed on hosted Macs; interactive
  macOS acceptance remains outstanding.

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
| §8 Cross-platform builds | Snap package, macOS bundle (universal build, signing, notarisation) and interactive Windows ARM64 GUI acceptance |
| §8 Accessibility | NVDA (Windows) and VoiceOver (macOS); Linux was checked with Orca |

## Published 1.0.0 verification

The published tag points to `6aa113f8b0042985955a7ca518f050d955f34910`.
All fourteen [CI jobs](https://github.com/themuddfamily/rustpy/actions/runs/40205175886)
passed on that revision: formatting, Clippy, builds, dependencies and test shards across
Linux, Windows and macOS. All 77 Python checks passed. The five type-check module tests
and a fresh isolated Linux headless startup smoke check also passed.

Both native [Windows packaging jobs](https://github.com/themuddfamily/rustpy/actions/runs/40205173038)
passed all 21 acceptance steps: portable CLI startup, silent Inno installation/reinstall/removal,
MSI core and full-feature installation, deleted-executable repair in a remembered custom
folder, integration checks and uninstall cleanup. Each executable is bound to recorded
linkage and retained runtime/source evidence. Independent review verified 4,021 evidence
files per architecture, the original linker records, all six download checksums and GitHub's
stored SHA-256 digests. The release includes `SHA256SUMS`.

Cancellation now stops both type-check syntax walks. The regression fails without the
checkpoints and passes with them; the unchanged local cancellation test measured 19.1 ms
p95 against 250 ms, and all three platform UI CI shards passed.

The full local quality gate is **not green**: the unoptimized code/completion typing-frame
check on the shared Linux host measured 83.5 ms p95 against 60 ms. The release notes disclose
this result. Packages are unsigned. Interactive Windows GUI, accessibility, embedded-browser
acceptance and older-version upgrades remain unverified. Linux, macOS and Store installers
are not published. Corpus, CPython oracle and fuzz gates still need recorded runs with their
provisioned inputs. Follow [docs/quality.md](docs/quality.md) and [docs/release.md](docs/release.md)
for subsequent releases.

## Continuous integration

`.github/workflows/ci.yml` runs fmt, Clippy, tests, the desktop build and cargo-deny on
Linux, Windows and macOS for every push to `main` and every pull request; all fourteen jobs
passed for the published 1.0.0 revision on 2026-10-25. Tests that depend on Unix tools, Unix fixture paths or behaviour
not yet understood on the hosted Windows and macOS runners are gated or ignored there, each
with its reason in the source and an entry in the known-issues list.
`.github/workflows/release.yml` is dispatched by hand: its default builds and tests native
Windows x64 and ARM64 packages. Linux/macOS builds, publication and Flatpak/Snap packaging
are opt-in inputs. The Microsoft Store (MSIX) steps stay disabled until the package identity is real.

## Known open issues and limitations

- **macOS** is untested on real hardware: app bundle, signing, notarisation, Finder
  integration, interpreter discovery and the WKWebView preview.
- **Windows terminal**: stopping a ConPTY-backed terminal session was the last Windows terminal
  issue found; the fix is integrated but has not been re-run on Windows. PowerShell, cmd and
  WSL terminal profiles, and zsh, fish and pwsh shells, have not been checked end to end.
- **Windows on Arm**: native CLI and installer checks passed on a hosted ARM64 runner;
  interactive GUI acceptance remains outstanding.
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
