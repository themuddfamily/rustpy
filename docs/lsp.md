# Language server (`rustpy lsp`)

`rustpy lsp` runs RustPy's Python intelligence as a [Language Server Protocol][lsp] server over
stdin/stdout, so editors such as Neovim, Helix, VS Code or Emacs get the same inspections,
completion, navigation, refactoring and formatting as the IDE. It opens no window.

```sh
rustpy lsp            # speaks LSP 3.17 on stdin/stdout
rustpy lsp --verbose  # also logs every message (debug level) to stderr and the log file
```

Only protocol messages are written to stdout. Log lines go to stderr and to the IDE log,
`<config>/log/rustpy.log` (see [configuration-directories.md](configuration-directories.md));
`RUST_LOG` sets the level as for the IDE.

[lsp]: https://microsoft.github.io/language-server-protocol/

## Features

| LSP request | What RustPy does (IDE feature name) |
|---|---|
| `publishDiagnostics` | Syntax errors and the IDE's inspections for open files: unresolved references and imports, unused imports and variables, argument lists, type checker, PEP 8, compatibility with the interpreter version, code smells, security, naming, docstring/signature mismatches, regular-expression errors, and spelling and grammar when they are enabled. Sent 0.3 s after the last change, like the editor. |
| `completion`, `completionItem/resolve` | Basic completion (names in scope, members after `.`, imports, keywords, overridable methods after `def`), filtered and ranked like the editor's lookup; names from other project modules and the standard library with their import (the editor's second Ctrl+Space) once three characters are typed; live templates and postfix templates as snippets. Resolve adds the Quick Documentation of the item. |
| `signatureHelp` | Parameter Info (Ctrl+P) with the active parameter and the overload count. |
| `hover` | Quick Documentation (Ctrl+Q): the signature, where the symbol comes from and the rendered docstring (reST, Google, NumPy and Epytext sections). Stub-only names take their docstring from the interpreter's sources. |
| `definition`, `declaration` | Go to Declaration (Ctrl+B), across files, into installed packages and into the bundled typeshed stubs (see *Stubs* below). |
| `typeDefinition` | Go to Type Declaration (Ctrl+Shift+B). |
| `implementation` | Go to Implementation(s) (Ctrl+Alt+B): subclasses of a class, overriding methods of a method. |
| `references` | Find Usages (Alt+F7) across the project, with semantic verification. Imports of the symbol count as usages even when declarations are excluded. |
| `documentHighlight` | Highlight Usages in File, with writes and reads told apart. |
| `prepareRename`, `rename` | Rename (Shift+F6) with the IDE's engine: every usage in the project, keyword arguments and docstring parameter entries. Library symbols, module imports and invalid names are refused with the IDE's message. Edits of open files carry their document version. |
| `formatting`, `rangeFormatting` | Reformat Code (Ctrl+Alt+L) for the file or the selected lines (see *Formatting*). |
| `documentSymbol` | The Structure view as a tree: classes with methods (`__init__` as a constructor, properties) and fields, functions with nested definitions, module-level variables and constants. |
| `workspace/symbol` | Go to Symbol: fuzzy (CamelHumps) matching over the definitions of every project file. |
| `foldingRange` | The editor's folds: imports, definitions, blocks, docstrings, brackets, `# region` comments. |
| `selectionRange` | Extend Selection (Ctrl+W) steps. |
| `codeAction` | The Alt+Enter actions that are plain text edits: quick fixes of the problems at the caret (Add import, create from usage, PEP 8 fixes, spelling corrections, …) as `quickfix`; intentions (invert `if`, convert quotes and f-strings, comprehension ↔ loop, import style, …) and *Insert documentation string stub* as `refactor.rewrite`; *Optimize imports* as `source.organizeImports`. Settings › Editor › Intentions switches are honoured. |
| `semanticTokens` (full, range) | The IDE highlighter's classification, refined with the scope model: parameters, local variables, classes, functions and methods, imported modules, decorators, `self`/`cls` (custom type `selfParameter`), builtins (`defaultLibrary`), docstrings (`documentation`). |
| `inlayHint` | Parameter names at call sites and inferred types of assignments (Settings › Editor › Inlay Hints). |
| `callHierarchy` | Call Hierarchy: callers (grouped by calling function or module) and callees. |
| Document sync | Incremental (`didChange` ranges are applied to the text; the analysis reparses only what changed and falls back to a full parse) and full-text changes; `didSave`, `didClose`. |
| Workspace | Multi-root workspace folders (`didChangeWorkspaceFolders`), `didChangeWatchedFiles` (registered dynamically when the client supports it), `didChangeConfiguration`, `$/cancelRequest`. |

Requests run on worker threads, so a slow Find Usages never blocks typing. A cancelled request
is answered with `RequestCancelled` right away. Position encodings UTF-8, UTF-16 and UTF-32 are
supported (UTF-8 is preferred when the client offers it).

Diagnostics use the inspection ids as codes (`PyUnresolvedReferences`, `PyUnusedImports`,
`PEP8:E302`, `PyArgumentList`, `SpellCheckingInspection`, `syntax`, …) with `source`
`RustPy`. Severities map as follows: Error → Error, Warning → Warning, Weak Warning and Typo →
Information, Information → Hint. Unused imports and locals, unreachable code and redundant
parentheses are tagged *unnecessary*, so editors grey them out as the IDE does. As in the
IDE, a `# noqa` or `# noinspection` comment silences the inspections of its line (unresolved
references and syntax errors are still reported).

## The project

Each workspace folder is a content root, like a project opened in the IDE. When the editor
opens a single file without a workspace, the file's folder becomes the project. From each
folder's `.rustpy/workspace.json` the server also takes the *Sources Roots* (added to import
resolution) and the *Excluded* folders (left out of Find Usages, Rename and Go to Symbol), and
the interpreter the IDE chose for the project.

The interpreter supplies `site-packages`, the Python version (for compatibility inspections and
stub version branches) and `sys.platform`. It is found as in the IDE: the project's recorded
choice, otherwise a virtual environment, otherwise the first interpreter found. Finding it runs
each candidate once, so the project's own environments (`.venv`, `venv`, …) are run only when
the IDE trusts the project (Trust Project, *Trusted Locations*, or `RUSTPY_TRUST_ALL`). For a
project that is not trusted, an in-project virtual environment is still used, read from disk
without running anything: its `pyvenv.cfg` gives the version and its `site-packages` folder
is searched. The editor's log (`window/logMessage`) says which interpreter is used.

Diagnostics wait up to four seconds for the interpreter at startup, so imports of installed
packages are not reported as unresolved before the search paths are known.

## Configuration

The server reads the IDE's `settings.json` (it never writes it), so the inspections switched on
in *Settings › Editor › Inspections*, the code style, the right margin, completion and inlay hint
options, the docstring format and disabled intentions apply as in the IDE. Your user dictionary
(`<config>/dictionary.txt`) and live templates (`<config>/live_templates.json`) are used too.

Editors can pass `initializationOptions`:

```json
{
  "interpreter": "/path/to/project/.venv/bin/python",
  "settings": {
    "inspections": { "spelling": false, "pep8": true },
    "editor": { "right_margin": 100, "inlay_type_hints": false },
    "code_style": { "quote_style": "double" }
  },
  "autoImportCompletions": true,
  "liveTemplates": true,
  "diagnosticsDelay": 300
}
```

| Key | Meaning |
|---|---|
| `interpreter` | The Python interpreter to use (it is run once to read its search paths). `"none"` uses the bundled stdlib stubs only. Without it, the interpreter is discovered as described above. |
| `settings` | Overrides merged over `settings.json`, with the same keys (see [configuration-directories.md](configuration-directories.md)). |
| `autoImportCompletions` | Offer names from other modules with their import (default `true`). |
| `liveTemplates` | Offer live and postfix templates as snippets (default `true`; needs client snippet support). |
| `diagnosticsDelay` | Milliseconds between the last change and the diagnostics run (default 300). |

`workspace/didChangeConfiguration` accepts the same object, either directly or under a
`rustpy` key; it is merged over the initialization options. Changing `interpreter` searches
again; any change re-checks the open files.

## Formatting

Formatting uses the IDE's builtin formatter (*Settings › Editor › Code Style*: quote style,
magic trailing comma, blank lines) with the file's indentation and right margin, decided as in
the editor: the client's tab size, then the indentation detected in the file (when *Detect and
use existing file indents* is on), then `.editorconfig` (`indent_size`, `indent_style`,
`max_line_length`). Files with `\r\n` line breaks keep them. A file with syntax errors is not
formatted (the request fails with the error's line).

When the code style selects an external formatter (Black, Ruff, autopep8, yapf), whole-document
formatting runs it with the project interpreter, as *Reformat Code* does, provided the project
is trusted; otherwise, and for range formatting, the builtin formatter is used.

## Stubs

Standard-library definitions live in typeshed stubs bundled into RustPy. To let the editor open
them, the server writes the stubs it navigates to into a cache directory: `rustpy/typeshed` in
the user cache directory (`$XDG_CACHE_HOME` or `~/.cache` on Linux, `~/Library/Caches` on macOS,
`%LOCALAPPDATA%` on Windows), or `$RUSTPY_CONFIG_DIR/cache/typeshed` when `RUSTPY_CONFIG_DIR` is
set. Requests on
an opened stub resolve inside the standard library, as in the IDE's read-only stub tabs. The
directory is safe to delete.

## Editor setup

Put `rustpy` on your `PATH` (or use the full path to the binary) in the examples below.

### Neovim

Neovim 0.11 or newer:

```lua
vim.lsp.config('rustpy', {
  cmd = { 'rustpy', 'lsp' },
  filetypes = { 'python' },
  root_markers = { 'pyproject.toml', 'setup.py', 'setup.cfg', 'requirements.txt', '.rustpy', '.git' },
  init_options = { diagnosticsDelay = 300 },
})
vim.lsp.enable('rustpy')
```

Neovim 0.10:

```lua
vim.api.nvim_create_autocmd('FileType', {
  pattern = 'python',
  callback = function(args)
    vim.lsp.start({
      name = 'rustpy',
      cmd = { 'rustpy', 'lsp' },
      root_dir = vim.fs.root(args.buf, { 'pyproject.toml', 'setup.py', '.rustpy', '.git' }),
    })
  end,
})
```

Semantic tokens use the standard types (`@lsp.type.parameter`, `@lsp.type.namespace`, …)
plus `@lsp.type.selfParameter` for `self` and `cls`.

### Helix

In `~/.config/helix/languages.toml`:

```toml
[language-server.rustpy]
command = "rustpy"
args = ["lsp"]
# Sent as initializationOptions:
config = { diagnosticsDelay = 300, settings = { inspections = { spelling = false } } }

[[language]]
name = "python"
language-servers = ["rustpy"]
```

### VS Code

VS Code needs a small extension to start a language server. With
[`vscode-languageclient`](https://www.npmjs.com/package/vscode-languageclient), the extension's
`package.json` declares `"activationEvents": ["onLanguage:python"]` and `"main":
"./extension.js"`, and `extension.js` is:

```js
const { LanguageClient } = require('vscode-languageclient/node');

let client;

exports.activate = function () {
  client = new LanguageClient(
    'rustpy',
    'RustPy',
    { command: 'rustpy', args: ['lsp'] },
    {
      documentSelector: [{ scheme: 'file', language: 'python' }],
      initializationOptions: { diagnosticsDelay: 300 },
    },
  );
  return client.start();
};

exports.deactivate = function () {
  return client && client.stop();
};
```

Disable other Python language servers (Pylance, Jedi) in the same workspace to avoid duplicate
diagnostics and completions.

### Emacs (Eglot)

```elisp
(with-eval-after-load 'eglot
  (add-to-list 'eglot-server-programs
               '((python-mode python-ts-mode)
                 . ("rustpy" "lsp" :initializationOptions (:diagnosticsDelay 300)))))
```

Then `M-x eglot` in a Python buffer (or add `eglot-ensure` to `python-mode-hook`).

## Limitations

- Only Python files (`.py`, `.pyi`) are analysed; notebooks and `untitled:` buffers are not.
- Diagnostics are pushed (`publishDiagnostics`) for open files; pull diagnostics are not
  implemented.
- Refactorings that need a dialog in the IDE (Extract, Inline, Change Signature, Move) are not
  offered as code actions.
- Cancellation stops waiting for a request at once, but a long project-wide search already
  running finishes in the background before its worker thread is free again.
