# Architecture

RustPy is a Rust workspace with a desktop egui application and headless CLI/LSP
entry points. The source tree, rather than this guide, defines exact API names.

| Boundary | Authority |
|---|---|
| `rustpy-text`, `rustpy-core` | Rope document, edit transactions, undo, file identity, cancellation and shared primitives |
| `rustpy-syntax` | Lossless tokens/CST, parser diagnostics and language syntax support |
| `rustpy-semantic`, `rustpy-analysis` | Project-aware scopes, inference, inspections, completion/navigation/refactoring and bounded analysis work |
| `rustpy-highlight`, `rustpy-textmate`, `rustpy-format` | Display spans/schemes, contributed grammars, source formatting/import policies |
| `rustpy-project`, `rustpy-index`, `rustpy-search` | Content roots and SDKs, persistent/shared indexes and scoped search |
| `rustpy-settings`, `rustpy-secrets`, `rustpy-net` | Serializable preferences, machine/project scope, credential storage and HTTP trust/proxy policy |
| `rustpy-run`, `rustpy-debug`, `rustpy-test` | Owned children/PTYs, DAP, tests/coverage/profiling/concurrency, cancellation/drain/reap |
| `rustpy-ui` | Editor state/view mapping, window/dialog/tool-window state and explicit dispatch into backends |
| `rustpy-cli`, `rustpy-lsp`, `rustpy` | Argument/IPC/headless policy, language-server transport and binary startup |
| Integration crates | VCS, terminal/console, packages, deployment, database, Docker, HTTP, AI, notebook and plugin protocols |

The document owns source text; a rendered row, folded/wrapped line or preview is
not a second editable source. Carets use source offsets mapped into visual rows.
Analysis reads a document/project revision. Completion, refactor previews, formatting
and background integration callbacks must revalidate the captured revision/path and
project authority before publication or mutation. Notebook cells and JSON views
share notebook authority and undo rather than racing separate file copies.

Project models distinguish project roots, content/module roots, SDK overrides and
language levels. Interpreter configuration and discovery metadata are not the same
as a successful process launch. Search and analysis scopes apply root exclusions
and ignored masks. Persistent indexes are replaceable derived data with source and
SDK identity; they cannot override a newer open document.

A background task captures its root, target, cancellation token and publication
authority. Cancellation suppresses stale publication, but physical retirement is
separate: owned processes must stop, drain and reap; HTTP/reader workers may remain
until the blocking API returns. Running-work inventory/quit coordination uses
stable owner IDs/stamps, not output progress or a cancelled delivery channel.
Services and tool windows display owned work without becoming another owner.
Project switching cancels old intent; completion refreshes only its captured root.

Trust is an admission policy, not merely a welcome-screen flag. Canonical project
identity is checked before execution and delayed callbacks. Remote/local file and
process operations retain their captured authority. Keys/passwords belong in the
password safe; portable preference sync excludes secrets, trust and machine paths.
Plugins have explicit capabilities, and collaboration has authenticated participants,
versioned host edits, permissions and revocation. Preview engines render active web
content only under their trust/root policy.

Native integration is intentionally conditional: eframe provides desktop windows,
optional Wry hosts embedded browser engines, platform process owners enforce child
cleanup, and system keyrings/fonts/input vary by OS. Source fixtures for an API do
not establish platform runtime support. See [quality gates](quality.md) for the
remaining native, interpreter, transport and installation matrix.
