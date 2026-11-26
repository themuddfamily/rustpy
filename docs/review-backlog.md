# Known issues

Open issues found in review and testing, with the area they affect. Platform coverage gaps
(interactive macOS and Windows on Arm, Snap, screen readers other than Orca) are listed in
[HANDOFF.md](../HANDOFF.md#known-open-issues-and-limitations). Please report new problems on
the [issue tracker](https://github.com/themuddfamily/rustpy/issues).

## Open

| Area | Symptom | State |
|---|---|---|
| Certificates (macOS) | Importing CAs that a user marked as trusted in their own (user-domain) keychain trust settings has not been checked on a live Mac: on the hosted runner `security add-trusted-cert` without `-d` waits for an authorization dialog that nobody can answer, and `security authorizationdb write` is refused. (Admin-domain trust, deny and certificates without trust settings are checked on CI against the live keychains; the user domain uses the same parser, and its precedence over the admin domain is unit-tested.) | Untested. |
| Window lifecycle test on Windows | `window_lifecycle::tests::prepared_guard_rechecks_current_workspace_domains` failed its "prepared" assertion once in a four-thread stress loop on the Windows CI runner and passed in every other run. The assertion now reports the quit progress and status message it received. | Unexplained; seen once. |
| Test process abort on Windows | One run of the `rustpy-ui` test binary in a four-thread stress loop on the Windows CI runner ended with exit code 0xC0000409 (process abort) without naming a test. | Unexplained; seen once. |

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
