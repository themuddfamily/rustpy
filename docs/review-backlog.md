# Known issues

Open issues found in review and testing, with the area they affect. Platform coverage gaps
(macOS, Windows on Arm, Snap, screen readers other than Orca) are listed in
[HANDOFF.md](../HANDOFF.md#known-open-issues-and-limitations). Please report new problems on
the [issue tracker](https://github.com/themuddfamily/rustpy/issues).

## Open

| Area | Symptom | State |
|---|---|---|
| Terminal (Windows) | Stopping a terminal session backed by ConPTY can leave its output reader running after the process has exited, so the session does not finish cleanly. | Fix in source; needs a re-run on Windows. |
| Frameworks (Pyramid, web2py) | For projects created by the New Project wizard, the framework run actions picked the conventional entry files (`development.ini`, `web2py.py`) instead of the generated `app.py` / `run_web2py.py`. | Fix in source; needs a check against real Pyramid and web2py installs. |
| Terminal profiles | PowerShell, cmd and WSL profiles, and zsh, fish and pwsh as shells, have not been exercised end to end. | Untested. |
| Certificates (Windows, macOS) | Importing trusted roots from the Windows certificate store and the macOS keychain is covered by tests but has not been checked against live system stores. | Untested. |

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
