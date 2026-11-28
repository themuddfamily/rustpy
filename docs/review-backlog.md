# Known issues

Open issues found in review and testing, with the area they affect. Platform coverage gaps
(interactive Windows on Arm, Snap, screen readers other than Orca) are listed in
[HANDOFF.md](../HANDOFF.md#known-open-issues-and-limitations). Please report new problems on
the [issue tracker](https://github.com/themuddfamily/rustpy/issues).

## Open

No open issues are known at present. New ones are listed here as a table of area, symptom
and state.

## Recently fixed

- Command-line launcher: on a loaded machine `rustpy <file>` could be refused as "Busy", and a
  window that had just switched projects could stay unregistered until its next repaint.
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
