# Known issues

Open issues found in review and testing, with the area they affect. Platform coverage gaps
(interactive Windows on Arm, Snap, screen readers other than Orca) are listed in
[HANDOFF.md](../HANDOFF.md#known-open-issues-and-limitations). Please report new problems on
the [issue tracker](https://github.com/themuddfamily/rustpy/issues).

## Open

| Area | Symptom | State |
|---|---|---|
| Window lifecycle test on Windows | `window_lifecycle::tests::prepared_guard_rechecks_current_workspace_domains` failed its "prepared" assertion once in a four-thread stress loop on the Windows CI runner and passed in every other run. The assertion now reports the quit progress and status message it received. | Unexplained; seen once. |
| Test process abort on Windows | One run of the `rustpy-ui` test binary in a four-thread stress loop on the Windows CI runner (at the notify 9 commit) ended with exit code 0xC0000409 (process abort) about 3 s before the run would have finished, with empty standard error and no test named. Not reproduced since: 1,924 full-suite four-thread runs on Windows at the current code (exit codes checked) and 2,065 more with procdump attached to every run to capture a crash dump (shown to capture Rust's abort), plus 850 at the commit of the original sighting, gave no abort; 90 Linux runs at four and eight threads, some with a 1.5 MB test-thread stack, gave none either. Ruled out: a test-thread stack overflow (on Windows it names the thread and exits 0xC00000FD; the deepest UI tests need between 1.5 and 1.75 MB of the 2 MB test-thread stack at `opt-level = 0`), a double panic or a panic in a no-unwind context (both write to the uncaptured standard error), and the corrupt-workspace stage watchdog (its 90 s limit could not elapse in a 56 s run). That watchdog did report through captured output, so a firing watchdog would have looked exactly like this; it now writes to the real standard streams. | Unexplained; seen once in about 4,900 Windows runs. A native fast-fail (C runtime invalid-parameter or stack-cookie check in a bundled C library) remains possible. |
| Store Python alias blocks process start on Windows | Interpreter discovery runs the Microsoft Store `python.exe` app-execution alias. On the Windows CI runner its `CreateProcessW` once never returned (waiting in the app-execution-alias activation), and because the Rust standard library serialises process creation on Windows, every other process start in the test binary (scripts, Git, interpreter probes) then waited behind it; the probe's 8 s timeout cannot apply before the process exists. Seen once in about 4,000 stress runs (`project_tab_discovery_transition_tests`). | Open. |
| Previous-project registration test on Windows | `launch::tests::returning_to_previous_project_after_failed_publication_retries_registration` fails "returning to the earlier project must retry the failed publication" in about 1 in 250 four-thread runs of the `rustpy-ui` tests on Windows, and once in 90 loaded Linux runs. | Open; timing-dependent. |

## Recently fixed

- HTML editing: completion could insert Python suggestions in HTML files and commit stale
  items while typing quickly, corrupting the saved text.
- Jupyter: viewing a DataFrame or array from a managed kernel failed with
  `ModuleNotFoundError: rustpy_scientific_table`.
- Floating tool windows that stay on top could cover the *Exit RustPy* confirmation.
- A window that started while another was shutting down could connect to the closing one.
- Version control: the Perforce revision graph lost the depot path; Mercurial history was
  missing descriptions, users and branches; the Subversion `svn:ignore` property was compared
  without its final newline.
- Built-in SSH: cloning with an encrypted private key failed before the passphrase prompt.
- Docker Compose: logs of fast containers were not drained to the end, and `$` in the effective
  configuration was escaped twice.
- The built-in web preview rejected its own URLs when the base path was empty.
- Device-code (OAuth) sign-in did not stop polling after expiry or cancellation.
- Many menu and dialog labels were missing from the translations.
- Search Everywhere could open while typing capital letters (two quick Shift presses).
- Unsaved notebook edits could be lost on window close, project switch or external reload, and
  a failed Save could leave formatting applied to the editor.
- Convert encoding, Undo, Save wrote the file with the wrong encoding.
- Proxies: a locked password store sent proxy requests without credentials; Deployment could
  write a newly typed FTP password into `deployment.json`.
