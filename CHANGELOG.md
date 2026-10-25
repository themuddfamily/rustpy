# Changelog

All notable changes to RustPy are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/). Early Access Program builds are tagged
`vX.Y.Z-eap.N` or `vX.Y.Z-beta.N` and published as GitHub pre-releases.

The section of a version becomes the notes of its GitHub release and the *What's New* page
that the IDE opens once after an update, so write the entries for users. When you release,
rename `[Unreleased]` to the new version and date, and start a new empty `[Unreleased]`
section above it (see [packaging/README.md](packaging/README.md#release-checklist)).

## [Unreleased]

## [1.0.0] - 2026-10-25

First public release. RustPy is new, so this section lists what the IDE can do rather
than changes against an earlier version. Linux is the best-tested platform. CI builds and
automated tests pass on Linux, Windows and macOS; native GUI and installer acceptance checks
remain incomplete.

### Added

- **Editor:** multiple carets and block selection, smart keys, folding, soft wrap, sticky
  lines, inlay hints, rainbow brackets, semantic highlighting, live, postfix and surround
  templates, macros, local history, scratch files, bookmarks, editor splits and floating
  editors, Markdown preview, image, hex and large-file viewers, EditorConfig support.
- **Python code insight** on RustPy's own lossless parser and type inference with bundled
  typeshed stubs: completion (basic, smart, auto-import), parameter info, quick documentation,
  about 60 inspections with quick fixes, intentions, spelling and grammar checks, regex
  injection, run-time type collection and a Black-compatible formatter.
- **Navigation and refactoring:** Search Everywhere, go to class, file, symbol and action, find
  usages, structural search and replace, type, method and call hierarchies, rename, extract,
  inline, change signature, move, pull up and push down, safe delete.
- **Run, debug and test:** run configurations, a debugpy-based debugger, pytest, unittest,
  doctest, behave and Twisted Trial runners, branch coverage, a profiler, a concurrency diagram,
  and a Python Console with variables, a DataFrame viewer and `# %%` cells.
- **Version control:** Git (commit, diff, log graph, branches, merge, rebase, stash, blame,
  conflicts, GitHub pull requests), plus Mercurial, Subversion and Perforce support.
- **Tools:** embedded terminal, Python Packages (pip or uv) with PyPI and OSV vulnerability
  checks, interpreter discovery and creation (virtualenv, conda, Poetry, Pipenv, uv, Hatch,
  PDM, SSH, Docker, WSL), HTTP client, database tools, Docker and Kubernetes, deployment over
  SFTP, rsync and FTP, tasks and contexts, an interactive features trainer.
- **Frameworks and data science:** Django, Flask and FastAPI support with endpoints and
  templates (Jinja2, Django), a Jupyter notebook editor, scientific table and plot viewers,
  Gherkin `.feature` files.
- **Language server:** `rustpy lsp` serves inspections, completion, navigation, rename and
  formatting to other editors.
- **AI Assistant:** chat with editor context, AI actions and commit-message generation through
  the Anthropic Messages API or a compatible Chat Completions provider.
- **Customisation:** Dark, Light and High Contrast colour schemes and UI themes, keymap presets,
  declarative plugins, settings sync, a UI in English and eight other languages, accessibility
  support through AccessKit.
- **Help › Check for Updates…** looks for a newer release on GitHub and shows its release
  notes, with **Download**, **Ignore This Update** and **Remind Me Later**.
- **Settings › Appearance & Behavior › System Settings › Updates**: check for updates on
  startup (at most once a day) on the *Stable Releases* or *Early Access Program* channel,
  the time of the last check, and the repository that releases come from.
- **Help › What's New in RustPy** shows the changes of the running version; it opens by
  itself once after RustPy is updated.
- Linux packages: `.rpm`, a Flatpak manifest, a Snap (classic confinement) and AppStream
  metadata for software centers; packages for 64-bit Arm (aarch64). *Open With* in file
  managers offers RustPy for Python files and Jupyter notebooks.
- Windows: builds for Windows on Arm (arm64). The installer can associate `.py`, `.pyw`,
  `.pyi` and `.ipynb` files with RustPy, add *Edit with RustPy* and *Open Folder as RustPy
  Project* to the Explorer context menu and add RustPy to `PATH`; the uninstaller removes
  all of it again. Packages include native-library licences and source material, and link
  the compiler runtime statically so no separate Visual C++ runtime installation is needed.
- macOS: one universal app for Apple silicon and Intel Macs. Finder's *Open With* lists
  RustPy for `.py`, `.pyi` and `.ipynb` files, and the app bundle contains a `rustpy`
  command-line launcher.
