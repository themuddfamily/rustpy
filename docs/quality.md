# Quality gates

This document lists the project's quality gates, the source that implements each one and what
a full verification run has to show. It is a plan for reproducing the checks, not a report of
results: several harnesses (the `qa/` crate, fuzz targets, benchmarks, UI goldens) have been
written but not yet run end to end. See [HANDOFF.md](../HANDOFF.md) for the current
verification status.

| Requirement | Existing or newly authored entry point | Remaining proof |
|---|---|---|
| Unit tests per crate | Workspace `cargo test`; inline `#[test]` and crate `tests/` | Run every crate, report counts/ignored tests; a crate with no executable tests is a coverage gap |
| CPython/parser corpus | `rustpy-syntax/tests/corpus.rs`; CPython oracle and mutation tests | Provision pinned CPython Lib/test and popular-package sources; record accepted/rejected/skipped counts |
| Typeshed/popular valid sources | `qa/tests/parser_sources.rs` includes `.py` and `.pyi`, no silent empty success | Provision pinned valid-source roots; triage every parse/encoding failure |
| Inference conformance subset | `qa/tests/typing_conformance.rs`; extensive `rustpy-semantic/tests/` controls | Execute subset and maintained semantic suite; no full upstream-conformance claim |
| Inspection goldens | Semantic inspection tests, syntax support JSON goldens, TextMate token goldens | Compare diagnostic code/severity/range/quick-fix outputs, review updates |
| Formatter | `rustpy-format/tests/black_corpus.rs`, pinned manifest/provenance and formatter unit fixtures | Run full corpus, idempotency and range/comment/import checks; retain Black-version provenance |
| Refactoring | `rustpy-refactor` fixtures and UI connected editor/refactor tests | Execute stale/read-only/multi-file/undo controls |
| UI snapshots/behavior | Headless egui `rustpy-ui` connected fixtures (`Context::run_ui`), accessibility/tree tests; golden paint digests (`rustpy-ui/src/ui_snapshot.rs`, `app/ui_snapshot_tests.rs`, `qa/goldens/ui/`) | Generate and review the first goldens, then run with `RUSTPY_REQUIRE_SNAPSHOTS=1`; native scripted screenshots and visual review on each platform |
| Fuzzing | `qa/fuzz/fuzz_targets/python.rs`: lexer/parser/lossless/formatter-idempotency; seed corpus | Provision nightly/cargo-fuzz; bounded serial run, triage/minimize crashes; not exhaustive proof |
| Benchmarks | `qa/benches/editor_pipeline.rs`, Criterion lexer/parser/formatter sizes | Record release hardware/toolchain/baseline/noise; million-line gate separately deferred |
| Platform matrix | CI on Linux/Windows/macOS (`ci.yml`); release architecture recipes (`release.yml`) | Actual native builds and GUI/input/process/keyring/font/browser checks on supported platforms |
| fmt / Clippy | Workspace commands below | Execute on integrated revision, fix concrete findings |
| Dependencies | `qa/deny.toml`: advisories/licenses/source restrictions | Provision cargo-deny and advisory DB, resolve findings, verify vendored/resources/corpus licenses separately |
| Release | [Release checklist](release.md), existing packaging/checksum fixtures | Authorized signed/notarized/native packaging and install/update/uninstall checks; no publication here |

The [per-crate source inventory](quality-inventory.md) lists declared test sites and
integration files per crate; it counts source, not executed coverage. Regenerate it with
`python3 scripts/update-quality-inventory.py` after larger changes.

## Running the gates

`scripts/quality.py` prints the command of a gate by default and runs it with `--execute`.
On Linux it restricts itself (and every child process) to two CPUs of the current affinity,
lowers its priority to nice 10, and exports `CARGO_BUILD_JOBS=2`, `RAYON_NUM_THREADS=2` and
`RUST_TEST_THREADS=2`. It also runs Cargo offline (`CARGO_NET_OFFLINE=true`), disables
Rustup automatic installation, and gives each run a fresh output directory with private
RustPy configuration, system, log and XDG directories, so a gate never reads or writes your
real IDE settings. The output directory is created under `$RUSTPY_QUALITY_DIR` when that is
set, otherwise under the system temporary directory, and is kept afterwards for inspection.
`CARGO_TARGET_DIR` is optional; without it the normal `target/` directory is used.

```sh
python3 scripts/quality.py fmt              # print the command only
python3 scripts/quality.py fmt --execute
python3 scripts/quality.py clippy --execute
python3 scripts/quality.py unit --execute
python3 scripts/quality.py docs --execute
python3 scripts/quality.py build --execute
python3 scripts/quality.py qa --execute
python3 scripts/quality.py deny --execute
```

Run the gates one after another, not in parallel. Dependencies must already be fetched
(`cargo fetch --locked`), because the gates run offline. `python3 -m unittest discover -s qa
-p 'test_*.py'` runs the Python checks of the packaging, licence and quality scripts.

The unit gate runs the cancellation and typing-frame latency cases in separate processes
with one test thread, after the ordinary workspace tests. All phases are required; any
failure fails the gate. The 250 ms cancellation and 60 ms typing-frame bounds are unchanged.
CI also runs cancellation separately in its UI shard; the existing hosted-runner guard for
typing-frame timings remains in place. Other test bodies cannot consume the local latency
measurements' CPU budget.

The separate `qa/Cargo.toml` and `qa/fuzz/Cargo.toml` manifests have their own lockfiles
(Criterion 0.5.1, libfuzzer-sys 0.4.13); shared dependency versions match the main lockfile.
cargo-fuzz with a nightly toolchain and cargo-deny with its advisory database must be
installed separately. Run `cargo fmt --manifest-path qa/Cargo.toml -- --check` (and the same
for the fuzz manifest) in addition to the workspace check. The licence policy in
`qa/deny.toml` allows the listed permissive licences and MPL-2.0; it is a technical check, not
legal advice.

The vendored GUI crates are excluded from the workspace and are tested separately, from the
repository root:

```sh
cargo test --offline --locked --manifest-path vendor/eframe/Cargo.toml --no-default-features --features default_fonts,glow,wayland,x11,persistence,accesskit --lib --config "patch.crates-io.egui_glow.path=\"$PWD/vendor/egui_glow\"" -- --test-threads=2
cargo test --offline --locked --manifest-path vendor/egui_glow/Cargo.toml --no-default-features --features wayland,x11 --lib -- --test-threads=2
```

The `egui_glow` patch is needed because a standalone vendor build does not inherit the
workspace's patch table.

### Corpora, fuzzing and benchmarks

For the valid-source corpus, set `RUSTPY_VALID_CORPUS_DIRS` (separated like `PATH`) to the
roots to parse, for example the bundled `crates/rustpy-semantic/typeshed/stdlib` plus
selected package and test directories, and run `python3 scripts/quality.py corpus --execute`.
Non-UTF-8 files are reported as failures there; do not point it at CPython's intentionally
invalid test files. The CPython oracle gates need `RUSTPY_CORPUS_DIRS` and
`RUSTPY_CORPUS_PYTHON` (an installed interpreter) and run as `oracle-valid` and
`oracle-mutation`. Every declared root must contain at least one source file; empty roots,
zero parsed sources or zero mutants fail the gate. Record corpus revisions, the interpreter
version and every exclusion.

`python3 scripts/quality.py fuzz --execute` runs a bounded 60-second libFuzzer session with a
fresh corpus and artifact directory in the gate's output directory; the tracked seeds under
`qa/fuzz/corpus/python` are read only. Minimise any crash into a small regression fixture.
`python3 scripts/quality.py bench --execute` runs the Criterion benchmarks; their numbers are
not a regression threshold until a baseline has been measured on known hardware.

## Native and integration gates

Headless egui tests do not exercise a native window. On Linux, GUI checks go through
`scripts/smoke.sh`, which starts a private Xvfb display, launches `./target/debug/rustpy` (or
`./target/release/rustpy` with `RUSTPY_SMOKE_PROFILE=release`) with software OpenGL and takes a
screenshot, so no window ever appears on your desktop. Use a fresh `RUSTPY_CONFIG_DIR`, and make
sure the binary you check is the one you just built. `RUSTPY_SMOKE_TIMEOUT`,
`RUSTPY_SMOKE_AWAIT_SCRIPT` and `RUSTPY_SMOKE_DEADLINE` lengthen scripted runs.

### Performance driver

`qa/performance/driver.py` prints its plan by default and runs it with `--execute`:

- **Cores and workers.** `--cores` is the CPU-affinity cap of the driver and all
  children (default 10); `--workers` sets the
  background analysis pool via `RUSTPY_BACKGROUND_THREADS` (default cores − 1, the
  IDE's own reservation of one core for the UI thread). The roadmap asks for
  "parallel analysis across cores" without naming a core count, so the million-line
  target is measured with the multi-core default; a 2-core run leaves one worker
  and is diagnostic only. Both values are recorded in `run.json`/`acceptance.json`.
- **Million.** The release test binary is built first with
  `cargo test --no-run --message-format=json` outside the 1 GB whole-tree watchdog
  (or supplied as `--test-binary`), then only that binary runs under the watchdog
  and the `--seconds` limit (default 300). `--memory-limit-bytes` adjusts the watchdog.
- **Build profiles.** `--profile debug|release|both` (`--binary` for debug,
  `--release-binary` for release). The roadmap latency/memory targets apply to the
  **release** build, which is what users run; with `both`, the debug result is
  reported alongside and labelled as not carrying the target.
- **Script steps.** The input script records `mark` steps in the trace; the capture
  waits for the whole script up to `--seconds` (60..900) and `--step-scale` stretches
  its waits. A step that never ran is reported as "not reached" and fails, as do the
  checks that depend on it (typing/completion, Power Save, Invalidate Caches).
- **Trace volume.** `--trace-max-rows` (default 1,000,000) caps the trace, and a
  capped trace ends with an explicit `trace_truncated` row that fails acceptance.
  Kinds in `--trace-count-only` (default `frame`) are only counted
  (`suppressed_counts` rows); `index` rows carry the frame total, so background
  indexing stays measurable.
- `cargo bench --locked` for `qa/` uses `qa/Cargo.lock`; the driver itself does not use
  that lockfile.

A scripted capture can use `--input-script qa/ui-smoke.txt --screenshot <capture.png>`
through `scripts/smoke.sh`; the script opens Find Action and Settings without running project
code. Review the capture at the scale, theme and font you care about. This path uses software
rendering only and says nothing about GPU presentation. By default a smoke run trusts the
fixture project, so never point it at a checkout you do not trust;
`RUSTPY_SMOKE_UNTRUSTED=1` exercises Safe Mode instead.

Windows and macOS need native runners for GUI, input and process-cleanup checks. Apply the
same two-CPU, low-priority limits to the whole process tree there (a Windows job object or
runner policy; the macOS runner's CPU allocation); `scripts/quality.py --execute` only
supports Linux.

Install only the integrations a gate exercises (Python versions, debugpy, pytest, coverage,
notebook kernels, SSH, Docker, WSL, database drivers, package managers, the
`native-web-preview` browser engine, keyring and signing tools) and record their versions.
Test the missing-tool and untrusted-project paths as well. Network or account tests need
isolated credentials and servers. When verifying cancellation, check that the worker actually
stopped, its output was drained and its child processes were reaped, not only that the view
was hidden.

`ci.yml` runs on every push to `main` and every pull request. `release.yml` is dispatched by
hand; publishing and sandbox packages are opt-in inputs, and the MSIX steps stay disabled
until the package identity is real.
