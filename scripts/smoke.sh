#!/usr/bin/env bash
# Runs the IDE headlessly on a virtual X display (never shows a window on the
# user's desktop) and captures a screenshot. Usage:
#   scripts/smoke.sh <project-dir> --open-file f.py --screenshot out.png [--run] [--tool git] [--terminal "cmd"]
#   [--input-script steps.txt]   replay mouse/keyboard input (see crates/rustpy-ui/src/input_script.rs:
#                                wait/move/click/dclick/down/up/drag/key/type/paste, coordinates in px)
#   RUSTPY_SMOKE_DELAY=<s>       seconds before the screenshot is taken
#   RUSTPY_SMOKE_UNTRUSTED=1     do not trust every project (exercise the trust dialog / safe mode)
#   RUSTPY_SMOKE_PROFILE=release run ./target/release/rustpy instead of ./target/debug/rustpy
#   RUSTPY_SMOKE_TIMEOUT=<s>     hard timeout of the IDE process (default 120)
#   RUSTPY_SMOKE_AWAIT_SCRIPT=1  take the screenshot only after the input script finished
#   RUSTPY_SMOKE_DEADLINE=<s>    latest screenshot time (default max(20, delay + 15))
set -euo pipefail
cd "$(dirname "$0")/.."
export WAYLAND_DISPLAY=
unset WAYLAND_DISPLAY
export WINIT_UNIX_BACKEND=x11
export LIBGL_ALWAYS_SOFTWARE=1
export RUSTPY_CONFIG_DIR="${RUSTPY_CONFIG_DIR:-${TMPDIR:-/tmp}/rustpy-smoke-config}"
if [ -z "${RUSTPY_SMOKE_UNTRUSTED:-}" ]; then
    export RUSTPY_TRUST_ALL=1
else
    unset RUSTPY_TRUST_ALL
fi
case "${RUSTPY_SMOKE_PROFILE:-debug}" in
    debug|release) profile="${RUSTPY_SMOKE_PROFILE:-debug}" ;;
    *) echo "RUSTPY_SMOKE_PROFILE must be debug or release" >&2; exit 2 ;;
esac
timeout="${RUSTPY_SMOKE_TIMEOUT:-120}"
case "$timeout" in
    ''|*[!0-9]*) echo "RUSTPY_SMOKE_TIMEOUT must be whole seconds" >&2; exit 2 ;;
esac
exec xvfb-run -a -s "-screen 0 1400x900x24" timeout "$timeout" "./target/$profile/rustpy" "$@"
