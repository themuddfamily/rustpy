# Authored quality harnesses

See [the quality map](../docs/quality.md). These are unexecuted source additions;
normal workspace tests do not automatically build this independent Cargo workspace.
No external corpus is downloaded. Fuzz seeds are original unfinished/valid editor
sources, not copied third-party files. The type-conformance subset is original and
intentionally bounded. Formatter runtime code remains owned by `rustpy-format`.

`Cargo.toml` owns semantic/corpus integration tests and Criterion benchmarks.
`fuzz/Cargo.toml` owns the libFuzzer target. `deny.toml` is applied to the main
workspace. `ui-smoke.txt` is a native scripted-input recipe, not a snapshot oracle;
the UI snapshot oracle is described below.
Use final verification to resolve dependencies, author lockfiles if retained, run
these harnesses, and triage failures without silently weakening assertions.

The final execution wrapper `scripts/quality.py` requires an existing shared target
and an existing nested `TMPDIR` inside an allocated RustPy temporary root. It
creates only isolated child gate directories, including RustPy/XDG settings and
logs, and never redirects Cargo to a new dependency cache. Cargo is offline and
Rustup automatic toolchain installation is disabled; provision reviewed dependencies,
nightly/cargo-fuzz, cargo-deny and its advisory database explicitly before execution.
The deny gate disables advisory fetching. Missing tools/data are failures, not skips.

Each declared valid-source corpus root must contribute a regular `.py`/`.pyi`
file; directories, symlinks and FIFOs are not source files. Oracle roots must
contribute `.py` files and require an explicit available `RUSTPY_CORPUS_PYTHON`.
Fuzzing writes its first/output corpus and crash artifacts in the isolated gate
directory; tracked repository seeds are secondary read-only inputs. Successful
formatting is checked for syntax, program/comment preservation and idempotence.
These are authored contracts, not measured conformance or execution evidence.

## UI snapshots

`crates/rustpy-ui/src/ui_snapshot.rs` renders IDE surfaces through the real `IdeApp`
drawing code in a headless `egui::Context` (no window, no GPU): fixed screen size,
1x scale, an explicit theme and only the bundled fonts. After a few settling frames
one simulated second apart, the captured frame's paint output is reduced to a text
digest — one line per visible rectangle/line/path/mesh and per text galley, with
whole-point positions, exact colours and merged style runs (font family/size/colour)
— and compared with `qa/goldens/ui/<name>.txt`. The same frame is rendered twice and
must digest identically, so nondeterministic surfaces fail loudly rather than flake.
Machine paths (the temporary fixture project) are replaced by `$PROJECT`, and
status widgets fed by background jobs or the host (interpreter, Git, indexing,
memory, updates) are hidden. Surfaces live in `crates/rustpy-ui/src/app/ui_snapshot_tests.rs`:
status bar and project view on a fixture project, the editor with highlighted
Python, the Settings › Editor › Code Folding page and the Go to Line dialog.

    cargo test -p rustpy-ui ui_snapshot                       # compare / create missing
    RUSTPY_UPDATE_SNAPSHOTS=1 cargo test -p rustpy-ui ui_snapshot   # rewrite goldens
    RUSTPY_REQUIRE_SNAPSHOTS=1 cargo test -p rustpy-ui ui_snapshot  # CI: missing fails

A missing golden is written and reported as new unless `RUSTPY_REQUIRE_SNAPSHOTS=1`
is set. A mismatch fails with a unified diff (golden → actual) and leaves the full
rendering as `<name>.new.txt` (git-ignored) for review; accept it by renaming or by
rerunning with `RUSTPY_UPDATE_SNAPSHOTS=1`. Review golden diffs like code: intended
UI changes (including new Settings pages in the tree) update them, regressions do not.
Goldens are not committed yet: they are generated and reviewed at the final
verification stage, with the resource caps in AGENTS.md and an isolated `TMPDIR`.

## Screen-reader acceptance (Orca, Linux)

`qa/accessibility/orca_acceptance.sh` runs the debug (or `RUSTPY_A11Y_PROFILE=release`)
binary under Orca on a private Xvfb display inside `dbus-run-session`, so the user's
display and accessibility bus are never touched. Speech-dispatcher output goes to the
ALSA `null` device; what Orca *would* speak is read from its `--debug-file`
(`SPEECH OUTPUT:` lines). Real X key events (xdotool) drive the editor, the Project
tree, editor tabs and Settings › Accessibility (including its "Test notification
announcement" live region), Shift+Tab wrapping inside the modal Settings dialog and a
named Settings › Appearance combo box read with its current value ("Language combo box
English", from the AT-SPI text of the combo box), and Tab / Shift+Tab staying inside the
modal Go to Line dialog; after each step the script also queries AT-SPI for the
focused object (role, name, states) and finally audits the whole tree for unnamed
interactive widgets. Results land in `$TMPDIR/orca-*/` (`report.json`,
`orca-debug.out`, `atspi-tree.txt`, `rustpy.log`; `RUSTPY_A11Y_SHOTS=1` adds a
screenshot per step). Needs `orca at-spi2-core speech-dispatcher
speech-dispatcher-espeak-ng python3-gi gir1.2-atspi-2.0 xvfb xdotool` (openbox optional).

    TMPDIR=/path/to/scratch qa/accessibility/orca_acceptance.sh

NVDA (Windows) and VoiceOver (macOS) are not covered.
