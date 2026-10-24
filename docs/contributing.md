# Contributing

Thanks for helping with RustPy. This page covers the workspace layout, coding conventions,
how to run the checks on a shared or small machine, and what a pull request should contain.

## Workspace layout

| Path | Contents |
|---|---|
| `crates/rustpy` | The binary: argument handling, logging, startup |
| `crates/rustpy-ui` | The egui application: editor view, tool windows, dialogs, settings pages |
| `crates/rustpy-*` | Engine and integration crates; see [architecture.md](architecture.md) |
| `crates/rustpy-semantic/typeshed` | Bundled typeshed stubs (third-party, do not edit by hand) |
| `vendor/` | Patched copies of `eframe` and `egui_glow`, excluded from the workspace |
| `qa/` | Standalone quality crate: corpus tests, benchmarks, fuzz targets, UI goldens |
| `packaging/` | Installers, package metadata and licence notice tooling |
| `scripts/` | Build, smoke-test, release and generator scripts |
| `docs/` | User guide and developer documentation |
| `roadmap.md` | Feature list and status; source comments cite it as `roadmap §x.y` |

## Coding conventions

- Format with `cargo fmt` and keep `cargo clippy --workspace --all-targets -- -D warnings`
  clean. The minimum toolchain is Rust 1.95 (edition 2021).
- Keep engine logic in the backend crates and out of `rustpy-ui`; the UI dispatches into them.
  The document owns the source text; views, previews and analyses work on a captured revision
  and must revalidate it before applying edits or publishing results.
- Long work (analysis, indexing, processes, network) runs off the UI thread, is cancellable
  and must not deliver results after cancellation.
- Describe behaviour in comments and user-facing text; refer to roadmap items as
  `roadmap §x.y` when a change implements one.
- Do not add new dependencies, downloaded corpora or generated build output without
  discussing it first. Every bundled third-party file needs its licence and an entry in the
  notices (see [packaging/README.md](../packaging/README.md#bundled-resource-notices)).
- User-visible strings go through `rustpy-i18n`; add the English text and leave other
  languages to translators when you cannot provide them.

## Tests and fixtures

Add tests at the narrowest public consumer that shows the behaviour:

- parser tests assert lossless round-tripping and the expected diagnostics;
- semantic fixtures exercise inference and, where relevant, navigation and completion;
- formatter fixtures assert valid output and idempotency;
- multi-file refactoring fixtures check every changed file and the rejection of stale or
  read-only targets;
- background work is tested for cancellation both before the worker starts and after it has
  delivered results.

A test that compares an algorithm with a copy of itself proves little. Goldens (`qa/goldens/`)
need a documented input and a reviewed update, never an automatic accept-all. Minimise fuzz
crashes into a small regression fixture. Screenshots must not contain secrets, private paths
or account data.

## Running checks with limited cores

A full workspace build is heavy. On a shared machine, cap every level of parallelism, because
Cargo's job limit alone does not limit test threads or Rayon pools:

```sh
export CARGO_BUILD_JOBS=2 RAYON_NUM_THREADS=2 RUST_TEST_THREADS=2
nice -n 10 taskset -c 0,1 cargo clippy -j 2 --workspace --all-targets -- -D warnings
nice -n 10 taskset -c 0,1 cargo test -j 2 --workspace -- --test-threads=2
```

`taskset` sets the CPU affinity, which child processes inherit. Run one build at a time and
reuse one target directory instead of building several checkouts in parallel.
`python3 scripts/quality.py <gate> --execute` applies the same limits automatically on Linux;
see [quality.md](quality.md) for every gate.

## Headless UI testing

Never open GUI windows on a desktop for automated checks. `scripts/smoke.sh` runs the IDE on a
private Xvfb display and saves a screenshot:

```sh
cargo build
RUSTPY_CONFIG_DIR=$(mktemp -d) scripts/smoke.sh <project-dir> --open-file main.py --screenshot out.png
```

`--action`, `--tool` and `--input-script` drive the IDE; see
[configuration-directories.md](configuration-directories.md#headless-smoke-tests-scriptssmokesh) and
`crates/rustpy-ui/src/input_script.rs`. Many UI behaviours are also covered by headless egui
tests inside `rustpy-ui`, which are preferable to screenshots because they assert results.

## Pull requests

- Keep a pull request to one feature or fix. Explain the observable behaviour before and
  after, the safety condition the change protects, the fixtures you added and the checks you
  actually ran. Never say a check passed because its test exists.
- Before opening it, run `cargo fmt --all -- --check`, Clippy and the tests of the crates you
  touched (the whole workspace for cross-crate changes), plus a smoke test for UI changes.
- Update the user guide in `docs/` when behaviour changes, and regenerate the
  [keymap reference](keymap.md) with `python3 scripts/update-keymap-reference.py` when
  actions or shortcuts change. Add an entry to [CHANGELOG.md](../CHANGELOG.md) for
  user-visible changes and update the status in [roadmap.md](../roadmap.md).
- Document limitations and required external tools rather than hiding an unsupported path
  behind a setting.
- Release preparation follows [release.md](release.md).

On macOS, run the UI tests with `RUSTPY_KEYMAP_PLATFORM=default` and a `TMPDIR` that is not a symlink (for example `$TMPDIR` set to a directory under your home): the keyboard tests are written against the Windows/Linux keymap and compare canonical paths.
