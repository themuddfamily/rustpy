# RustPy maintainer status

This page is for whoever picks up RustPy maintenance: what is finished, what is partial, what
still has to be verified for future releases, and the known open issues. Feature
detail lives in [roadmap.md](roadmap.md); user-facing issues are listed in
[docs/review-backlog.md](docs/review-backlog.md).

## Summary

- Version 1.0.0. [CHANGELOG.md](CHANGELOG.md) has a dated `1.0.0` section for the first public
  release. [RustPy v1.0.0](https://github.com/themuddfamily/rustpy/releases/tag/v1.0.0)
  is published with Windows x64 and ARM64 setup, MSI and portable packages plus checksums.
- Roadmap: 291 feature items, 284 done (`[x]`), 7 partial (`[~]`), none untouched.
  Remaining work covers platform integration, packaging, NVDA/VoiceOver, performance targets
  and recorded corpus/oracle/fuzz runs.
- Linux is the primary development platform and the best-tested one. Windows 11 (x64) has been
  checked for interpreters, browsers and the web preview. Native x64 and ARM64 installer
  automation passed. macOS builds and automated tests passed on hosted Macs; interactive
  macOS acceptance remains outstanding.

## Implemented features

Everything in the roadmap marked `[x]`: the editor, Python code insight (parser, type
inference, inspections, intentions, completion, navigation, refactorings, formatter), run and
debug, the test runners, coverage and profiler, Git and the other VCS integrations, the
terminal and Python Console, packages and interpreters, Jupyter notebooks and the scientific
viewers, the HTTP client, database tools, Docker, Kubernetes and remote interpreters,
deployment, frameworks (Django, Flask, FastAPI, Jinja2 and others), plugins, localisation, the
language server and the AI Assistant.

The published-release verification below records the current evidence and remaining gates.

## Partial

| Roadmap item | What is missing |
|---|---|
| §6 Interpreter discovery | macOS framework and Homebrew interpreter paths not verified on a Mac |
| §6 Add Interpreter dialog | macOS flows not verified |
| §6 Web browsers and preview | Safari and the WKWebView built-in preview not verified |
| §8 Cross-platform builds | Snap package, macOS bundle (universal build, signing, notarisation) and interactive Windows GUI acceptance |
| §8 Performance targets | Interactive GUI latency is unmeasured; the headless typing-frame gate passes locally (see below) |
| §8 Accessibility | NVDA (Windows) and VoiceOver (macOS); Linux was checked with Orca |
| §8 Quality | Recorded corpus, CPython oracle and fuzz runs with provisioned inputs remain outstanding |

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

The local typing-frame gate passes on an otherwise idle host (2026-11-25, two cores):
the code/completion frame measured 34-41 ms p95 against the 60 ms bound in the test
profile, and 22-28 ms in a release build, where plain typing measured 10-12 ms p95.
The 83.5 ms figure recorded for 1.0.0 was taken while the shared host was under load.
These are headless frame timings; latency in the interactive GUI has not been measured.
Packages are unsigned. Interactive Windows GUI, accessibility, embedded-browser
acceptance and older-version upgrades remain unverified. Linux, macOS and Store installers
are not published. Corpus, CPython oracle and fuzz gates still need recorded runs with their
provisioned inputs. Follow [docs/quality.md](docs/quality.md) and [docs/release.md](docs/release.md)
for subsequent releases.

## Continuous integration

`.github/workflows/ci.yml` runs fmt, Clippy, tests, the desktop build and cargo-deny on
Linux, Windows and macOS for every push to `main` and every pull request; all fourteen jobs
passed for the published 1.0.0 revision on 2026-10-25. Tests that depend on Unix tools or Unix
fixture paths are gated to the platforms that have them. The tests that were ignored on
Windows or macOS for behaviour not then understood were re-enabled in November 2026 with
their causes fixed; what remains open is in the known-issues list. CI also provisions fish,
a WSL distribution, pinned Pyramid and web2py installs and a trusted test CA so the
terminal-profile, framework and certificate checks run against the real thing.
`.github/workflows/release.yml` is dispatched by hand: its default builds and tests native
Windows x64 and ARM64 packages. Linux/macOS builds, publication and Flatpak/Snap packaging
are opt-in inputs. The Microsoft Store (MSIX) steps stay disabled until the package identity is real.

## Known open issues and limitations

- **macOS** builds and automated tests passed on hosted Macs. Native app packaging, signing,
  notarisation, interactive Finder integration, interpreter discovery and the WKWebView
  preview remain unverified.
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
