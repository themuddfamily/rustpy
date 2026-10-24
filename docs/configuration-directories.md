# Configuration directories

RustPy keeps its state in a few places: a per-user configuration directory, a per-project
`.rustpy` folder and a log directory (roadmap §8, "Configuration directories"):

| What | Where | Scope |
|---|---|---|
| Settings, templates, schemes, plugins, generated scripts | the configuration directory: `$RUSTPY_CONFIG_DIR` / `--config-dir`, otherwise `~/.config/rustpy` on Linux | per user |
| System state and Local History | `$RUSTPY_SYSTEM_DIR` / `--system-dir`, otherwise the configuration directory | per user |
| Logs, crash reports and diagnostic captures | `$RUSTPY_LOG_DIR` / `--log-dir`, otherwise `<system>/log` | per user |
| Window size and position | `app.ron` in the platform data directory; `RUSTPY_CONFIG_DIR` does not move it | per user |
| Symbol indexes (interpreter libraries, projects, shared bundles) | `<config>/index` when `RUSTPY_CONFIG_DIR` is set, otherwise the platform cache directory (`~/.cache/rustpy/index` on Linux) | per user, rebuilt when missing |
| Open files, tool-window layout, run configurations, data sources, histories | `<project>/.rustpy/` | per project |
| Content roots and folder marks (shareable through version control) | `<project>/.rustpy/project.json` | per project, shared |
| Headless smoke tests | `${TMPDIR:-/tmp}/rustpy-smoke-config`, set by `scripts/smoke.sh` | per test run |

RustPy needs none of these files to start. A missing file means defaults, and deleting the
configuration directory resets the IDE.

## The configuration directory

`rustpy_settings::Settings::config_dir()` picks the directory each time it is called:

1. `RUSTPY_CONFIG_DIR`, used as given when set. Give an absolute path, because a relative
   one is resolved against the IDE's working directory. Smoke tests, CI and the portable
   Windows launcher use it to keep their state apart from yours, for example
   `RUSTPY_CONFIG_DIR=/tmp/try-rustpy rustpy ~/project`.
2. Otherwise, the platform's configuration directory (`dirs::config_dir()`) plus `rustpy`:

   | Platform | Directory |
   |---|---|
   | Linux, BSD | `$XDG_CONFIG_HOME/rustpy` when `XDG_CONFIG_HOME` is an absolute path, otherwise `~/.config/rustpy` |
   | macOS | `~/Library/Application Support/rustpy` |
   | Windows | `%APPDATA%\rustpy`, that is `C:\Users\<you>\AppData\Roaming\rustpy` |

3. If neither exists (no home directory), `rustpy` relative to the working directory.

Files and subdirectories are created when they are first written:

```
<config>/
├── settings.json          IDE settings
├── live_templates.json    your live templates
├── fileTemplates.json     your file and code templates
├── font_cache.json        the installed fonts, as found at the last start
├── colors/<name>.json     your color schemes
├── plugins/               installed plugins (Settings › Plugins)
├── certs/                 accepted and imported server certificates (PEM)
├── passwords.db           encrypted password database (when the OS keyring is not used)
├── passwords.db.lock      lock file for changes to it from several windows
├── passwords.key          its random key, when it has no master password
├── log/rustpy.log         default log location (overridable)
├── local_history/         default Local History location (overridable)
└── interpreters/          wrapper scripts for remote interpreters (SSH, Docker, WSL)
```

### `settings.json`

All IDE-level settings, written by `Settings::save()` as pretty-printed JSON. The file is
written to `settings.json.tmp` first and then renamed over the old one. RustPy saves it
when you press *Apply* or *OK* in the Settings dialog, when you open or trust a project,
every 30 seconds while it runs (eframe's auto-save) and on exit. Changes in the Settings
dialog take effect while you edit (pages with unapplied changes are marked with a dot);
*Cancel* or closing the dialog puts everything back, and the auto-save writes the state
from before the dialog's unapplied changes.

Several RustPy windows (processes) can share the file. Each save merges with what is on
disk: a value this process changed since it loaded or last saved the file wins; every other
value keeps the file's version (written by another window or by hand), and
`recent_projects`, `trusted_projects` and `trusted_locations` are combined. A hand edit
therefore survives unless the running IDE changes the same value afterwards. Two saves at
the very same moment can still race.

Every section has defaults and unknown keys are ignored, so an old or partial file still
loads and new settings appear with their default values. A file that is not valid JSON, or
has a value of the wrong type, is first copied to `settings.json.invalid-<unix time>` next to
it; RustPy then starts with the defaults, says so in an error notification that stays until
it is closed ("Settings not loaded", with a link that opens the saved copy), and
replaces the file at its next save. A file broken while RustPy runs is copied the same way
before it is overwritten.

Top-level keys:

- `appearance`: theme (`Dark`, `Light` or `High Contrast`), UI font size and family
  (`ui_font_family`, an installed font; empty for the default), tool window bars, status bar, toolbar, the editor
  schemes last used with the dark and the light theme (`dark_scheme`, `light_scheme`, used
  by Quick Switch Scheme), the UI `language` (a locale code such as `zh-CN`, `ja` or `de`;
  empty for the system language: the first translated language of the GNU `LANGUAGE` list,
  else the language of the message locale (the first of `LC_ALL`, `LC_MESSAGES` and `LANG`
  that is set), else English; as in gettext, `LANGUAGE` counts only when that locale is set
  and is not `C`. Only these environment variables are read, not the Windows or macOS
  language settings, so there the default is usually English), the memory indicator in the status bar, and the not yet supported
  navigation bar and compact mode switches;
- `editor`: font (`font_family`: `Hack` is the bundled font, any other installed
  family can be named; `enable_ligatures`), color scheme name, tab and indent sizes, line numbers, whitespace, indent
  guides, right margin and visual guides, soft wrap, inlay hints, caret, folding and
  save-time cleanup;
- `keymap` (the preset: `Default`, `macOS`, `VS Code`, `Eclipse` or `Sublime Text`),
  `custom_shortcuts` (your changes to that preset: action id and shortcuts, an empty list
  unbinds) and `keymap_overrides` (the changes of the other presets, which come back when
  that preset is selected again);
- `inspections` (per-group switches) and `disabled_intentions`;
- `terminal` (shell, virtualenv activation) and `vcs` (Git executable, confirm before push);
- `macros` (recorded editor macros) and `quick_list` (actions in the Quick List popup);
- `code_style`, `copyright`, `completion`, `refactoring`, `docstring_format` and
  `semantic_highlighting`;
- `external_tools` (`name`, `group`, `program`, `arguments`, `working_dir`, `show_console`
  and an optional `shortcut` such as `Ctrl+Alt+Shift+B`; a key the keymap binds stays with
  its action, editor actions included, and a key without Ctrl or Alt other than F1–F35 is
  ignored), `file_watchers` and `browser`;
- `notifications`: `disabled_groups`, the notification groups whose balloons are turned off
  ("Don't show again" on a balloon, or *Settings › Appearance & Behavior › Notifications*);
  their notifications still go to the Notifications tool window;
- `recent_projects` (newest first, at most 25), `reopen_last_project`, `confirm_exit` (ask
  before exiting; RustPy always asks while run, debug, test, console or terminal sessions are
  alive),
  `show_tips_on_startup`, and `open_project_in` (`ask`, `current` or `new`: where File ›
  Open, Recent Projects, Get from VCS and a dropped folder open a project while another one
  is open; *New Window* starts another RustPy process);
- `trusted_projects` (project roots trusted with Trust Project) and `trusted_locations`
  (*Settings › Build, Execution, Deployment › Trusted Locations*: every project below one
  of these absolute directories is trusted). Trust is stored only here: nothing inside a
  project, such as `.rustpy/workspace.json`, can mark it trusted;
- `ai` (*Tools › AI Assistant*: model, output tokens, base URL, `remember_api_key`). The API
  key itself is in the password safe; a plain-text `api_key` written by older versions is
  moved there at startup and removed from the file;
- `http_proxy` (*Appearance & Behavior › System Settings › HTTP Proxy*): `mode`
  (`no_proxy`, `auto_detect`, the default, which follows `HTTPS_PROXY` / `HTTP_PROXY` /
  `ALL_PROXY` / `NO_PROXY`, or `manual`), `kind` (`http` or `socks`), `host`, `port`
  (0 = 8080 or 1080), `no_proxy` (host names, `.domain` suffixes, `*` wildcards, IP
  addresses and CIDR ranges), `authenticate`, `login`, `remember_password` and the last
  `check_url`. The proxy password is in the password safe; until it is known in a session
  (read from the safe, or typed in), requests reach the proxy without credentials rather
  than with an empty password. These settings apply to PyPI and vulnerability lookups, the
  AI Assistant, the HTTP Client and issue trackers; a manual proxy is also passed to pip and
  uv. Git and FTP deployment (curl) only see the proxy variables of RustPy's environment;
- `certificates` (*Tools › Server Certificates*): `accept_automatically` and
  `use_system_store`: also trust the operating system's certificate authorities, that is
  the CA bundle file (`SSL_CERT_FILE`, `SSL_CERT_DIR`, `/etc/ssl/certs/…`), on Windows the
  Trusted Root Certification Authorities store (including roots deployed by group policy),
  restricted to roots whose effective enhanced key usage allows TLS server authentication
  and which are absent from the Windows Disallowed store,
  and on macOS the keychain certificates that the admin or user trust settings
  trust without policy, hostname, application or key-usage restrictions (user
  settings override administrator settings). Scoped OS trust cannot be represented
  in RustPy's TLS root store, so those certificates must be imported explicitly;
- `passwords` (*Appearance & Behavior › System Settings › Passwords*): `storage` (`native`,
  `encrypted_file` or `memory`) and `database` (empty = `<config>/passwords.db`). Settings
  imported with another storage move the saved passwords there, as *Apply* does.

### `certs/`

Server certificates RustPy trusts beyond the bundled Mozilla roots and (unless switched
off) the operating system's certificate authorities, one PEM file per certificate named after the start
of its SHA-256 fingerprint. Comment lines at the top say how it is trusted:

- `# Host: <name>`: the certificate was accepted when a connection asked about it
  ("Untrusted Server's Certificate"). Exactly this certificate is then accepted for exactly
  these hosts, like a browser's certificate exception; it does not make the server a
  certificate authority for other names.
- `# Trust: certificate authority`, and any PEM or DER file you put into the folder
  yourself: a trust anchor. Certificates it issued are trusted for the names they are
  issued for (a company's root CA, imported with *Import* on the Server Certificates page).

Removing a file (or *Remove* on the page) withdraws the trust.

### `passwords.db` and `passwords.key`

The password safe keeps the AI Assistant API key, the HTTP proxy password and FTP
deployment passwords (`rustpy-secrets`). With *In native keyring* (the default) they are in
the Secret Service keyring (GNOME Keyring, KWallet, KeePassXC) on Linux or the login
Keychain on macOS, as items labelled `RustPy: <key>`, or in the Windows Credential Manager
as generic credentials named `RustPy/<key>`; when the keyring cannot be used they go to the
encrypted file instead and move into the keyring once it works again. `passwords.db` is JSON with an AES-256-GCM encrypted body. Its key is
either derived from a master password (PBKDF2-HMAC-SHA256, 600 000 iterations; RustPy asks
for it once per session) or, without a master password, the 32 random bytes of
`passwords.key`. Both files are owner-only (mode 0600 on Unix). Deleting `passwords.key`
makes a database without master password unreadable. Neither file is included in File ›
Manage IDE Settings exports, and an import never replaces them. RustPy windows are separate
processes: each change to the database runs under a lock on `passwords.db.lock` (next to the
database; the file stays), so changes from several windows at once all land. When one window
changes the master password, the other windows ask for the new one.

While the database is locked by a master password that was not entered yet, work that
needs a saved password (a deployment upload, the Deployment dialog) asks for it and waits;
a password saved meanwhile is used for the session and stored once the database is
unlocked. It is never written into `.rustpy/deployment.json`.

### `live_templates.json`

Your live templates, written by *Settings › Editor › Live Templates*. The file is a JSON
array; the builtin templates are compiled into `rustpy-templates` and are not in it. A
template in this file replaces the builtin template with the same abbreviation, and
*Remove / Restore builtin* deletes your version again.

### `fileTemplates.json`

Your file and code templates, written by *Settings › Editor › File and Code Templates* (the
*Default* scheme) and by *Tools › Save File as Template*. The file is a JSON object with
`templates` and `includes` arrays; each entry has `name`, `extension`, `file_name` (the name
the *New* dialog proposes), `body`, `enabled` (listed in the Project view's *New* menu),
`reformat` and `description`. The builtin templates and the *File Header* include are
compiled into `rustpy-templates`; an entry with a builtin's name replaces it, and *Reset*
removes the entry again. A project can have its own `.rustpy/fileTemplates.json` (the
*Project* scheme), whose entries win over these. The *Default* scheme on the settings page
lists only the builtins and this file, so editing a template there never copies a
project's version into it; the *Project* scheme shows what the project uses, and its
*Reset* goes back to the *Default* scheme's template.

Template bodies use a subset of Velocity: `${NAME}`, `${FILE_NAME}`, `${CLASS_NAME}`,
`${PACKAGE_NAME}`, `${DIR_PATH}`, `${PROJECT_NAME}`, `${USER}`, `${DATE}`, `${TIME}`,
`${YEAR}`, `${MONTH}`, `${DAY}` and more (listed on the settings page), `#parse("File
Header.py")`, `#if (${VAR})` … `#else` … `#end`, `#set ($VAR = "…")` and `## comments`.
`${DS}`, `$$` or `\$` write a `$`, `\#` a `#`. The *File Header* include writes the
copyright notice (*Code Style › Copyright*) as a comment when *Add to new files* is on.
Rendering stops at 8 MB of work or 64 `#parse` includes (a template that parses itself
would otherwise never finish): the *New* dialog then reports the template instead of
creating the file. *File › New File…* creates the typed name as it is and empty unless a
template is picked; a template creates only files with its extension (a `py` template also
`.pyw`), and adds the extension to a name that has none.

### `font_cache.json`

The installed font families (files, faces, weights) and the fallback fonts chosen for
Chinese, Japanese, Korean, Arabic, Hebrew, Thai, Devanagari and other scripts the bundled
fonts lack. RustPy scans the installed fonts on a background thread at every start and
rewrites the file; at the next start it installs the chosen editor and UI fonts from the
file at once instead of after the scan. The file is safe to delete.

### `colors/<name>.json`

Color schemes saved with *Save As…* on *Settings › Editor › Color Scheme*. `settings.json`
refers to a scheme by its file name without `.json` (`editor.color_scheme`). The builtin
schemes, Dark, Light and High Contrast, are compiled in and take precedence over a file with
the same name. Settings written by older builds that use the previous
built-in scheme names still load and are mapped to Dark and Light.

### `plugins/`

Plugins installed with *Settings › Plugins* (from the Marketplace or with *Install Plugin
from Disk…*), one directory per plugin named by its id, each with a `plugin.json` manifest
(format: crate `rustpy-plugins`). `plugins/state.json` records which plugins are disabled;
plugins it does not mention are enabled. Plugins only contribute data: live templates
(a user template with the same abbreviation wins, then the first plugin), color schemes
(a file in `colors/` with the same name wins), keymaps, external tools (run only when you
pick them), file templates and TextMate grammars (for files without a built-in highlighter
whose extension the plugin declares; your TextMate bundles win).
Changes in *Settings › Plugins* apply without a restart; a plugin's scheme or keymap in use
falls back to the default when the plugin is disabled or uninstalled. Hidden `.staging-*`,
`.old-*` and `.state-*` entries belong to an install in progress; leftovers older than an
hour are cleaned up (the old version of an upgrade that was interrupted half-way is put
back). Deleting a plugin's directory uninstalls it. *File › Manage IDE Settings* bundles do
not include plugins, and importing one never changes them: install plugins on the other
machine in *Settings › Plugins*.

Plugin tools use the same direct program runner as External Tools and File Watchers.
Macros are filled after arguments are split, so quotes, shell syntax and newlines in a
file name remain literal argument text. `$PluginDir$` paths are validated after quote
removal, and executable permissions are set during installation. Literal dollar signs
in an installed plugin's path are preserved; in External Tools templates, `$$` writes
one literal dollar sign.

### `log/rustpy.log`

Everything the IDE logs (the `log` crate through `env_logger`) goes to stderr and is
appended to this file. `RUST_LOG` sets the level: the default is `info`; try
`RUST_LOG=debug rustpy` or `RUST_LOG=rustpy_semantic=trace`. The file is never rotated, so
delete it when it grows. Windows release builds have no console window, so the file is their
only log. Smoke runs add lines such as `screenshot saved to …`, `smoke: unknown action id …`,
`smoke: expand …` and `smoke: find query = …`.

### `local_history/`

Snapshots of files, taken when a file is saved, before Reformat Code, Code Cleanup,
refactorings (also of files that are not open) and structural replace, before reverting to a
snapshot, and before reloading a file changed on disk. Snapshots are also taken from disk
before operations that rewrite or remove files: Git Rollback, Delete of an unversioned file,
Reset --hard, Stash, Unstash, Pull / Update Project, branch and revision checkout, merge,
cherry-pick, revert, merge-conflict resolution and Abort, and before deleting files or
folders (Project view Delete, refactorings). Folders are copied recursively, skipping
`.git`, virtualenvs, `site-packages`, caches and `node_modules`, binary files and files over
2 MB, and at most 5000 files or 64 MB per operation. The layout is:

```
local_history/<project-hash>/<file-hash>/<unix-time-ms>_<label>.txt
local_history/<project-hash>/<file-hash>/meta.json      {"path": …, "deleted": …}
```

Both hashes are 16 hex digits: a 64-bit hash of the absolute project root (`no-project` for
files opened without a project) and of the file path. They come from Rust's default hasher,
so a RustPy built with another Rust version may not find older snapshots. `meta.json`
records the file's path, so *Local History › Show History* on a folder in the Project view
lists every file below it with snapshots, including deleted ones, and restores them
(snapshots from before `meta.json` existed only show in the file's own history). In
`<label>`, every character other than a letter or digit becomes `-` (`Before-Reformat`). A
snapshot identical to the newest one is skipped, and only the newest 100 snapshots per file
are kept. Browse and revert them with *Show Local History*. The directory is safe to delete.

### `tools/` (older versions)

Older versions wrote a `<name>.sh` script here each time an External Tool or File Watcher
ran. Tools now run their program directly, with one argument per entry of the argument
template, so nothing is written any more and the directory is safe to delete.

### `interpreters/`

`<kind>-<target>.sh` wrapper scripts (kind `ssh`, `docker` or `wsl`), created by
*Tools › Add Remote Interpreter…*. They run the remote Python with the IDE's arguments. The
project interpreter recorded in `.rustpy/workspace.json` points at the script, so deleting
it breaks that interpreter.

## Window state outside the configuration directory

eframe saves the window position and size, plus some UI state, to `app.ron` in its own
directory. `RUSTPY_CONFIG_DIR` does not change this location:

| Platform | File |
|---|---|
| Linux | `$XDG_DATA_HOME/rustpy/app.ron` when that is an absolute path, otherwise `~/.local/share/rustpy/app.ron` |
| macOS | `~/Library/Application Support/rustpy/app.ron`, the same folder as the configuration directory |
| Windows | `%APPDATA%\rustpy\data\app.ron`, inside the default configuration directory |

Delete the file to reset the window geometry. The Python Console helper, the test reporter,
coverage data and the profiler runner also create short-lived files in the system temporary
directory.

## The symbol index directory

Go to Class / Go to Symbol / Go to File with *Include non-project items*, Search
Everywhere, second-press completion and the *Import* quick fix find library names in symbol
indexes (crate `rustpy-index`, IDE side in `crates/rustpy-ui/src/symbol_index.rs`). They
live in `<config>/index` when `RUSTPY_CONFIG_DIR` is set (smoke tests, portable installs),
otherwise in `dirs::cache_dir()/rustpy/index`: `~/.cache/rustpy/index` on Linux,
`~/Library/Caches/rustpy/index` on macOS, `%LOCALAPPDATA%\rustpy\index` on Windows.

```
index/
├── libraries/<hash>.rpidx   one per interpreter (hash of the interpreter path): its sys.path directories
├── projects/<hash>.rpidx    one per project (hash of the project root): its Python files
└── shared/<name>.rpidx      shared index bundles imported in Settings › Tools › Shared Indexes
```

An index records, for every file, its path, size, modification time and content hash, and
the file's classes, functions, methods, variables, imports (with a one-line signature) and
`__all__`. A file starts with an uncompressed header (format and schema version, JSON
metadata shown in Settings), followed by a zlib-compressed body. A file written by another
format or schema version, or damaged, is not used: the index is rebuilt. Indexes are
written to a temporary file and renamed, so several RustPy windows can share them.

When a project opens, its index and its interpreter's index are loaded and refreshed on
background threads (the status bar shows the progress). A refresh only reads files whose
size or modification time changed, and only parses those whose content hash is in no index
yet. The project index is refreshed after the file watcher reports changes; the library
index when a `sys.path` directory changes (a package was installed or removed), every 15
minutes, and from Settings (*Refresh*, *Rebuild*, *Delete*). *File › Invalidate Caches*
rebuilds both.

A shared bundle (*Export* on the Settings page) carries the symbols of the standard library
(optionally of site-packages too) by content hash, without local paths. With *Include
site-packages* every package installed there is exported, private ones included; other
`sys.path` entries (the sources of editable installs, `PYTHONPATH`) and the import hooks that
build backends put in site-packages for editable installs, named after your projects
(setuptools' `__editable__*`, hatchling's and pdm's `_editable_impl_*`, meson-python's and
scikit-build-core's `_<project>_editable*` modules), never are. Imported, or placed in a `shared-indexes` folder next
to the RustPy executable, it lets another installation build its library index without parsing
the files it contains. Bundles come from anywhere, so loading one is bounded: a file larger than
any index (about 256 MB), a body that expands more than 8× its compressed size beyond its first
4 MB, or data after the body is refused before it can take much memory. The whole directory is
safe to delete.

## The per-project `.rustpy/` directory

| File | Written by | Contents |
|---|---|---|
| `workspace.json` | the IDE, when another project is opened, every 30 seconds and on exit (`rustpy_settings::Workspace`) | open files (caret line and column, scroll line, pinned), the active file, expanded folders in the Project tree, tool window layout (`tool_windows`: whether the left tool window is shown and which one, Project, Structure or Bookmarks, its width, the bottom tool window and its height, floating or maximized, and the tool window used last, for Jump to Last Tool Window), restored when the project opens, the project interpreter, line breakpoints, watches, bookmarks (line, mnemonic, description), recent files and startup tasks. Older versions also listed Sources Roots and excluded directories here; they are read (and kept) here until the project structure is first changed, which writes them to `project.json`. A `trusted` key written by older versions is ignored: trust lives in `settings.json`. In safe mode the recorded interpreter is kept as it is, because the project's own virtual environments are not probed until the project is trusted |
| `project.json` | Mark Directory as, *Settings › Project › Project Structure* (`rustpy_settings::project::ProjectModel`) | the project structure: `content_roots` (folders attached to the project besides its directory), `source_roots`, `test_roots`, `resource_roots`, `template_dirs`, `namespace_packages` and `excluded`. Paths are relative to the project directory (`src`, `../shared`) unless they share no folder with it. Sources and Test Sources roots and content roots are import roots and go on `PYTHONPATH` of runs; Run All Tests runs the Test Sources roots; HTML and text files in Template Folders open as Jinja2 / Django templates; excluded folders are left out of Find in Files, Go to File, Search Everywhere, Find Usages, refactorings, Structural Search and the TODO list. Content roots are watched for new, deleted and renamed files like the project directory; a content root that contains the project directory (`..`, `/`), lies inside it or overlaps another content root is ignored with a warning. The file is reloaded when it changes on disk (a pull); one that cannot be parsed (a merge conflict) is reported and leaves the structure empty until it is fixed, and RustPy never overwrites it: Mark Directory as and the Project Structure page change nothing until then. Opening a project never writes this file; it is created when the structure is first changed |
| `runConfigurations.json` | Run/Debug Configurations (`rustpy_run::RunConfigStore`) | `configs` and the `selected` configuration name; temporary configurations are not saved |
| `dataSources.json` | Database tool window (`rustpy_db`) | an array of data sources: `{"name": …, "kind": {"type": "sqlite", "path": …}}` |
| `http-history.log` | HTTP Client (`rustpy_http::history`) | one JSON object per executed request: `time` (Unix seconds), `name`, `method`, `url`, `status`, `elapsed_ms`, `size` and `error`. Requests marked `# @no-log` are not recorded; the tool window shows the newest 50 |
| `fileTemplates.json` | *Settings › Editor › File and Code Templates*, *Project* scheme | the project's file templates and includes, in the format of `<config>/fileTemplates.json`; they win over the IDE-level ones |
| `console_history.txt` | Python Console (`rustpy_console::History`) | the input history, oldest first, at most 1000 entries; a multi-line input is stored on one line, joined with `␞` (U+241E) |
| `coverage/<configuration>.coverage`, `coverage/html/<configuration>/` | Run with Coverage (`rustpy_run::coverage`) | the coverage.py data file of the last coverage run of each run configuration (kept for the Coverage tool window's *HTML Report* and *Import…*), and the HTML report written from it |

Notes:

- When RustPy starts without a path and does not reopen the last project, it opens the
  working directory if that looks like a project: it contains `.rustpy`, `.git`,
  `pyproject.toml`, `setup.py`, `requirements.txt`, `.idea` or a `*.py` file.
- `.rustpy/` is hidden in the Project tool window, skipped by Find in Files and test
  discovery, and ignored by the file watcher, except that a change to `project.json` reloads
  the project structure.
- For version control, `workspace.json`, `http-history.log`, `console_history.txt`,
  `coverage/`, `tasks.json`, `runtime_types.json` and `deployment.json` (which may hold
  passwords) are personal state, while `project.json`, `runConfigurations.json` and
  `dataSources.json` can be shared with a team. This repository ignores
  `/.rustpy/workspace.json`. When the New Project wizard runs `git init`, the `.gitignore` it
  writes ignores everything in `.rustpy/` except those three shared files. The first time
  `project.json` is written, RustPy also adds a `.rustpy/.gitignore` that ignores everything
  there except itself and the three shared files, so personal files (and files that later
  versions add) stay out of version control.
- A missing or unreadable file means defaults. An unreadable file is overwritten the next
  time RustPy saves that file (`workspace.json` every 30 seconds, the others when they change),
  except `project.json`, which is left for you to fix.

## Environment variables

| Variable | Effect |
|---|---|
| `RUSTPY_CONFIG_DIR` | the configuration directory (see above) |
| `RUSTPY_SYSTEM_DIR` | system state and Local History; defaults to the configuration directory |
| `RUSTPY_LOG_DIR` | log, crash and capture directory; defaults to `<system>/log` |
| `RUSTPY_TRUST_ALL` | any value: treat every project as trusted, with no trust prompt and no safe mode. `scripts/smoke.sh` sets it unless `RUSTPY_SMOKE_UNTRUSTED` is set |
| `RUSTPY_SMOKE_UNTRUSTED` | `scripts/smoke.sh` only: any non-empty value leaves `RUSTPY_TRUST_ALL` unset, to test the trust dialog and safe mode |
| `RUSTPY_SMOKE_DELAY` | seconds to wait before the `--screenshot` capture (default 2.5) |
| `RUSTPY_DAP_TRACE` | any value: print the Debug Adapter Protocol traffic to stderr (not to the log file) |
| `RUST_LOG` | the `env_logger` filter (default `info`) |

## Headless smoke tests (`scripts/smoke.sh`)

`scripts/smoke.sh <project-dir> [flags]` runs the debug build on a virtual X display, so no
window ever appears on your desktop, and exits after taking a screenshot. It needs `xvfb`
(`apt-get install xvfb`) and a debug build (`cargo build`). The script:

- changes to the repository root first, so relative paths in the arguments are relative to
  the repository;
- unsets `WAYLAND_DISPLAY` and sets `WINIT_UNIX_BACKEND=x11` and `LIBGL_ALWAYS_SOFTWARE=1`
  (software OpenGL);
- sets `RUSTPY_CONFIG_DIR` to `${TMPDIR:-/tmp}/rustpy-smoke-config` unless it is already set, and sets
  `RUSTPY_TRUST_ALL=1` (unless `RUSTPY_SMOKE_UNTRUSTED` is set);
- runs `xvfb-run -a -s "-screen 0 1400x900x24" timeout 120 ./target/debug/rustpy "$@"`: a
  1400×900 virtual screen, and the IDE is killed after 120 seconds.

A smoke run still writes the project's `.rustpy/workspace.json` and eframe's `app.ron` (set
`XDG_DATA_HOME` to keep the latter out of `~/.local/share`).

The binary (`rustpy_ui::run`) understands these arguments. They work in normal sessions too;
`--screenshot` is what turns a run into a smoke test.

| Argument | Effect |
|---|---|
| `<path>` | The project directory to open. For a file, its directory becomes the project and the file is opened. |
| `--open-file <file>` | Open a file in the editor. A relative path that does not exist relative to the working directory is resolved against the project. |
| `--goto <line>:<col>` | Move the caret in the active editor (1-based; a missing or invalid number means 1). |
| `--run` | Run the active Python file, like *Run context configuration* (Ctrl+Shift+F10): pytest for `test_*.py` and `*_test.py`, otherwise the script with its existing run configuration (for example one set to *Run with Python Console*), or a new temporary one. |
| `--welcome` | Show the Welcome screen. |
| `--action <id>` | Queue an action by id. Repeatable. The ids are those of `Action::ALL` in `crates/rustpy-ui/src/actions.rs`, for example `SaveAll`, `ShowSettings`, `$Undo`, `ReformatCode` or `LocalHistory.ShowHistory`. Queued actions start 1.2 seconds after launch and run one every 0.25 seconds, pausing while the debugger runs. |
| `--tool <name>` | Open a tool window or a dialog (see below). |
| `--terminal "<cmd>"` | Open the Terminal tool window and type `<cmd>` and Enter about a second after launch. Ignored when `--tool console` selects the Python Console; use `--tool pyconsole:` there. |
| `--screenshot <png>` | Smoke mode. Once `RUSTPY_SMOKE_DELAY` seconds have passed and the queued actions have run, capture the window into the PNG, save the workspace and settings, and exit. After 20 seconds it gives up (`screenshot timed out`), so delays above 20 never capture. |

Values of `--tool`:

- tool windows: `git`, `gitlog` (the Git tool window's Log tab), `run`, `terminal`,
  `problems`, `debug`, `tests`, `packages`, `hierarchy`, `services`, `coverage`, `http`,
  `database`, `diagram`, `profiler`, `todo`, `notifications` and `console` (the Python
  Console);
- `settings:<page>`: the Settings dialog on `keymap`, `font`, `scheme`, `templates`,
  `filetemplates`, `interpreter`, `inspections`, `shared-indexes`, `proxy`, `passwords`,
  `certificates`, `project-structure` or `notifications` (any other value opens Appearance);
- `ssr:<pattern>`: Structural Search with the pattern filled in and searched;
- `pyconsole:<code>`: start the Python Console with the project interpreter (or `python3`)
  and run the statements in `<code>`, separated by `;;`. A statement `table:<expr>` opens
  the SciView table for `<expr>`;
- `expand:<abbr>`: type `<abbr>` at the caret and expand it as a live template;
- `breakpoint:<file>:<line>`: add a line breakpoint;
- `find:<query>`: Find in Files with the query;
- `everywhere:<query>`: Search Everywhere with the query.

Examples:

```sh
cargo build
scripts/smoke.sh /tmp/proj --open-file main.py --run --screenshot /tmp/run.png
scripts/smoke.sh /tmp/proj --open-file main.py --tool settings:keymap --screenshot /tmp/keymap.png
scripts/smoke.sh /tmp/proj --open-file main.py --goto 3:1 --action SaveAll --tool find:TODO --screenshot /tmp/find.png
scripts/smoke.sh /tmp/proj --tool 'pyconsole:rows = [{"a": 1, "b": 2}];;table:rows' --screenshot /tmp/table.png
RUSTPY_SMOKE_DELAY=6 scripts/smoke.sh /tmp/proj --terminal "python3 --version" --screenshot /tmp/terminal.png
```


Proxy bridge connections are accepted only when the client socket belongs to the
RustPy process. If the operating system cannot verify ownership, the bridge
refuses the connection. Ownership checks support Linux, macOS and Windows;
bridged HTTP authentication, IPv6 HTTP proxies and SOCKS proxies require one of
these platforms. The encrypted password database also refuses changes on file
systems without advisory file locking, instead of risking concurrent writes
from separate RustPy windows.

System CA support adds eligible native roots to the bundled Mozilla roots. It is
not the operating system's complete certificate verifier: OS distrust does not
remove an independently bundled Mozilla root or an explicitly imported IDE
certificate. The system-trust checkbox controls native additions; the Mozilla roots remain
available.

## Diagnostic tools and local data

*Help › Diagnostic Tools* provides local log collection, Activity Monitor,
thread dumps, IDE CPU recordings and the existing stack-trace analyzer.
*Help › Report a RustPy Issue* lets you write and review a browser issue draft;
build/runtime/OS information is optional. No files are attached automatically.

The log rotates at 5 MiB, keeping three old files. `--verbose` enables debug
logging; `RUST_LOG`, for example `rustpy_ui=debug,rustpy_run=info`, selects module
categories. Unhandled Rust panics save bounded local JSON reports with a
symbolicated Rust backtrace under `<log>/crashes`. Native signal/minidump and
platform-specific symbol availability are not implied by these panic reports.
Collection bundles the existing logs and panic reports with a manifest of
omissions; it does not gather settings, credentials, environment or projects.

Activity Monitor displays the process CPU delta (100% is one core), resident
memory, and running/recent background tasks, including hidden work. Worker CPU
is the task's own thread at progress checkpoints; parallel helper work is part
of overall process CPU and is not assigned to individual tasks. Cancellation is
cooperative and a stopping task remains owned until it finishes.

Install/configure an appropriate debugger (`gdb`, `lldb`, or Windows `cdb`) and
[samply](https://github.com/mstange/samply) under *Settings › Tools › Diagnostic
Tools*. Dump Threads attaches the debugger to this IDE process and collects
all thread backtraces. CPU Profile records this process at 49 Hz for the
configured 1–60 seconds. Both are explicit actions; neither runs at startup or
in scripted smoke sessions. OS attachment permissions and external tool support
still apply. A watchdog interrupts timed-out captures, then terminates the owned
process tree if it cannot finish. Cancel and shutdown retain ownership through
retirement. Profiles are validated as bounded Firefox-profile JSON before the
viewer is offered. Opening a profile starts a supervised local `samply load`
viewer in Run; Stop manages that session normally. Captures stay under
`<log>/diagnostics` and can contain local source paths or symbols.

*Settings › Appearance & Behavior › System Settings › Data Sharing* controls
local Productivity Guide action counts, disabled by default. Only bounded stable
action IDs and counts are stored; code, filenames and typed text are excluded.
The same checkbox appears in Productivity Guide, which can reset counts.
RustPy has no automatic diagnostic or usage-statistics upload.
