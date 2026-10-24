# Command-line launcher

Run `rustpy install-launcher` or **Tools → Create Command-line Launcher…** to
install a script for the current RustPy executable. An explicit filename chooses
another name, for example `rustpy install-launcher ~/.local/bin/rpy`
(`rpy.cmd` on Windows). Every name accepts the same arguments. Existing unrelated
files require `--force`. Ensure the reported launcher directory is on `PATH`.

```sh
rustpy project/
rustpy --line 42 --column 7 project/main.py
rustpy --wait project/main.py
rustpy diff before.py after.py
rustpy merge theirs.py ours.py base.py resolved.py
```

File requests go to the existing instance for the configuration directory.
`--new-window` creates a separate window. `--wait` returns when the opened tabs
or project close. Paths named like commands use `./test`, `./format`, etc.
The merge command returns 0 after Apply, 1 after Cancel, or 2 for invalid input.

## CI and headless execution

`--headless` is a global guard: it rejects IDE, diff and merge commands with
status 2, including unrecognized command names that would otherwise open paths.
It creates no window and forwards no GUI request. `format`, `inspect`, `test`,
`http`, `lsp`, help, version, diagnostics, completions and launcher installation
already run without a window; the guard is optional for these commands.

```sh
rustpy --headless format -r -s style.xml src tests
rustpy --headless format -r -dry src
rustpy --headless inspect . 'Project Default' reports -d src -v2 -format sarif
rustpy --headless test --project . --framework pytest --json --junit results.xml -- -q
rustpy --headless test --project . --framework unittest tests
rustpy --headless http requests.http --env staging --json
```

Formatting accepts `-s`/`-settings`, `-r`/`-R`, `-m`/`-mask`, `-charset`,
`-d`/`-dry` and `-allowDefaults`. Dry format exits 1 when changes are needed.
Inspections accept XML, JSON, plain text and SARIF output, a profile name or file,
`-changes`, `-d` directory and `-v0`/`-v1`/`-v2`. See each command's `--help`
for its report and exit-status policy.

### Tests

`test` runs the same `rustpy-test` reporter and result protocol as the IDE.
Targets use paths or node IDs such as `tests/test_math.py::TestMath::test_add`.
No targets means the project suite. `--project DIR` defaults to the current
directory. Arguments following `--` are exact framework argv, including embedded
spaces and `--option=value`; they are never interpreted by a shell.

`--framework` accepts `auto`, `pytest`, `unittest`, `doctest`, `nose`, `nose2`,
`trial`, `behave`, `tox` and `lettuce`. Auto respects the configured test runner,
project configuration and target detection. Automatic imports run in an owned
helper process, within the same cancellation/deadline boundary as tests.

Interpreter priority is `--interpreter`, the target module SDK, the project SDK
or saved workspace interpreter, local `.venv`/`venv`, then `python3`/`python` on
`PATH`. Mixed module SDKs require separate runs or an explicit interpreter.
Relative interpreter paths resolve against the project. A missing configured SDK
is an error rather than a silent switch to a different environment.

Project/settings preferences supply pytest arguments, custom pytest path,
capture policy and unittest discovery pattern/top-level directory. Project source
and test roots feed `PYTHONPATH`. `--env NAME=value` can be repeated and overrides
defaults; `--no-inherit-env` clears inherited variables. `--python-option ARG`
can be repeated for interpreter argv. `--cwd DIR` follows the test engine's
working-directory support (behave and Trial run at project root).

`--json` produces one result object on stdout containing framework, interpreter,
project, per-test results, summary, launcher exit code and cancellation/timeout
flags. Console output goes to stderr. Setup failures produce an error object.
`--junit FILE` additionally writes checked, atomic JUnit XML, including captured
output and synthetic runner failures for cancelled, empty or invalid runs.
The report path is relative to the launcher's working directory.

`--timeout SECONDS` is a positive total deadline including framework detection.
Ctrl-C/Break on Windows and SIGINT/SIGTERM/SIGHUP on Unix request cancellation.
Owned process groups/Windows jobs stop descendants before the leader is reaped;
buffered reporter events are drained with a bounded post-stop wait.

| Status | Meaning |
| --- | --- |
| 0 | At least one test ran, no failures/errors, clean runner exit |
| 1 | Tests failed or runner exited unsuccessfully after reporting tests |
| 2 | Invalid arguments, setup, reporter/protocol or report-writing failure |
| 5 | No tests ran |
| 124 | Deadline expired |
| 130 | Cancelled by a termination signal |

## Editor integration and directories

`rustpy lsp` speaks LSP over stdin/stdout; logs go to stderr or the log file.
`--disable-plugins` disables user-installed extensions. `--config-dir`,
`--system-dir` and `--log-dir` override their respective directories before
settings or services load. Their environment equivalents are
`RUSTPY_CONFIG_DIR`, `RUSTPY_SYSTEM_DIR` and `RUSTPY_LOG_DIR`.

## Shell completions

```sh
rustpy completions bash > ~/.local/share/bash-completion/completions/rustpy
rustpy completions zsh > "${fpath[1]}/_rustpy"
rustpy completions fish > ~/.config/fish/completions/rustpy.fish
rustpy completions powershell >> "$PROFILE"
```

All four scripts include HTTP and CI test commands and `--headless`. After
installing a launcher under a custom name, register or rename its shell completion
as appropriate for that shell. `rustpy help COMMAND` prints the full command help.

Tests for headless refusal, exact argument passing, JUnit escaping, settings,
the unittest protocol and timeouts live in the CLI and test crates.
The optional launcher-process fixture uses `RUSTPY_CLI_EXE` to locate the built
binary and verifies SIGTERM cancellation without display-server variables.
