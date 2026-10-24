# Getting started

Open a directory from the welcome screen or pass it to the launcher. RustPy
tracks the project root separately from interpreter libraries, VCS roots and
module content roots. Project Structure defines module SDK/language overrides;
a run configuration can explicitly override the interpreter again.

A newly opened, untrusted project starts in Safe Mode. Reading and editing are
available; project execution, probes, startup tasks and integration actions need
trust. Trust only directories whose contents and configuration you control.
Trust is checked again when project identity changes. Command-line execution is
also subject to its documented trust boundary; see [the launcher](../command-line.md).

Select Python through Settings › Project › Python Interpreter. Existing local
environments and configured remote descriptors are distinct from creating a new
environment. Environment creation uses the selected manager executable; venv,
uv, Conda, Poetry, Pipenv, Hatch and PDM require their corresponding tools when
selected. Remote SSH, Docker and WSL choices require their host tools and access.
An invalid saved interpreter is reported instead of silently becoming another SDK.
Background discovery or environment creation can be cancelled; a result belonging
to an old project must not select the current project's interpreter.

Create or open a Python file, edit it, then save. Actions on Save and File Watchers
are separately configured policies: a successful changed write can trigger selected
format/import/watcher/upload actions. Automatic execution is gated by trust.
External tools and watchers execute programs; inspect arguments, scope and output
paths before enabling them. Directory encoding rules affect decoding, while the
explicit status-bar Reload/Convert operation controls an already opened file.

Use Run to create a configuration from the current file or gutter. Select an
interpreter with the required project dependencies first. Missing debugpy, pytest,
coverage, notebook kernels, database drivers or external managers are installation
requirements, not bundled guarantees. Stop ends owned work; closing a window with
running work presents its running-process decision. A cancellation request can
remain in the process inventory until the actual worker or child has retired.

For native embedded HTML preview, build with `native-web-preview` and provision
platform browser-engine dependencies (WebKitGTK on Linux, WebView2 on Windows,
WKWebView on macOS). The ordinary system-browser action remains available without
that optional host. Browser profiles can select programs and arguments.
