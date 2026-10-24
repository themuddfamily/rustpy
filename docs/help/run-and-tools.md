# Run, debug and tools

A run configuration captures target, working directory, interpreter, environment,
parameters and workflow options. Module SDK selection applies unless an explicit
run interpreter overrides it. Shared project configurations and workspace-local
configurations have different persistence. Before-launch tasks run in order and
must finish successfully; a cancelled prerequisite must not launch its target.
Environment inheritance can be disabled. Inspect explicit environment variables
when reproducing CI behavior.

The Run tool window retains output and process ownership. Console settings include
encoding, bounded retention, stdout/stderr display, ANSI colors, wrapping, folding,
search and export. Emulate Terminal uses a PTY where supported. Input, interrupt
and stop have different meanings; Stop owns termination and retirement, while
clearing the screen only changes retained display. Services links to owned work
rather than inventing another process. Pinned output can outlive a finished run.

The debugger uses debugpy and the Debug Adapter Protocol. Breakpoints, frames,
variables, watches and evaluation depend on the selected Python and adapter.
Attach targets a real process and may be restricted by OS permissions or Python
injection support. Installing or configuring debugpy is an explicit operation.
Tests use pytest or unittest and their result protocol; the headless `rustpy test`
command supplies CI exit status and optional structured/JUnit results. Coverage,
profiler, concurrency analysis and thread-dump tools have their own captured targets
and cancellation lifecycles. None is evidence of performance until measured.

Python Console is an interpreter session; Terminal is a shell/PTY session. They
have separate histories, state and command semantics. Terminal shell integration
is shell/platform dependent. Closing or stopping a live session must retire its
owned process tree. A rendered prompt or cancellation flag is not proof of exit.

External Tools and File Watchers capture program, argv, macros, working directory,
scope and environment before dispatch. Project/global watcher definitions merge
under their configured rules. Save/debounce/external-change triggers are serialized
and coalesced to limit recursive execution. Generated outputs are refreshed only
for the captured project/path. Watcher templates still require installed tools.
SSH external tools require SSH configuration and remote mappings; secrets belong
in the password safe, not argument strings or shared settings.

HTTP requests can contact real services. Database consoles can mutate real data.
Deployment can overwrite remote files. Review target, environment and permissions
before executing these tools. Docker, SSH, remote gateways, Jupyter, package
managers and provider-specific database/VCS drivers require their configured
binaries, credentials or server components. Trust and cancellation checks remain
in force for delayed/background actions.
