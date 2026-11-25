# Known issues

Open issues found in review and testing, with the area they affect. Platform coverage gaps
(interactive macOS and Windows on Arm, Snap, screen readers other than Orca) are listed in
[HANDOFF.md](../HANDOFF.md#known-open-issues-and-limitations). Please report new problems on
the [issue tracker](https://github.com/themuddfamily/rustpy/issues).

## Open

| Area | Symptom | State |
|---|---|---|
| Terminal profiles | WSL profiles and fish as a shell have not been exercised end to end: the hosted CI runners have no WSL distribution and no fish. (cmd, Windows PowerShell, pwsh, bash and zsh profiles are tested end to end on CI.) | Untested. |
| Certificates (macOS) | Importing CAs that an administrator or user marked as trusted in the macOS keychain trust settings has not been checked against a Mac that has such settings; the CI runner has none. (The Windows store, the macOS system roots and the trust-export parser are tested against live stores.) | Untested. |
| External formatters on macOS | Launching an external formatter (Black, Ruff, …) fails with "Operation not permitted" on the macOS CI runner at the process-group step; the process tests are Linux-only until this is reproduced on a Mac. | Formatting |
| Directory diff on macOS and Windows | The symlink-safe directory inventory uses Linux descriptor-relative opens; on other systems directory comparisons report themselves incomplete. | Diff |
| Window registry and session store on Windows | Concurrent registration stress and the atomic session-store claim hit sharing violations (ERROR_ACCESS_DENIED) when a lease or record is replaced while another process reads it; the multi-process tests are Unix-only until the Windows lease protocol is reworked. | Windows |
| Closing a project with a corrupt workspace file on Windows | The test that closes a project whose `.rustpy/workspace.json` is corrupt hung for an hour in one Windows CI run and passed in others; ignored there until the hang is reproduced with a debugger. | Windows |
| Backup-save retry on Windows | The regression test for retrying a save after a blocked backup directory does not write the edited file on Windows. | Ignored on Windows pending investigation. |
| Stopping a Git job on macOS | A stopped Git job whose SSH helper ignores SIGTERM ends immediately on the macOS runner instead of waiting out the grace period; the process-group observation via proc_pidinfo needs checking on a Mac. | VCS |
| rsync deployment on stock macOS | Apple's rsync 2.6.9 lacks `-s` (protect-args); install rsync 3 from Homebrew. Remote listing also needs GNU find on the server. | Deployment |
| Local typing performance | An unoptimized code/completion frame check on the shared Linux host measured 83.5 ms p95 against a 60 ms budget. | Local performance gate not met; release GUI latency remains unverified. |

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
