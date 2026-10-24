#!/usr/bin/env bash
# Headless Orca screen-reader acceptance (Linux, AT-SPI via AccessKit).
#
# Requires: orca, at-spi2-core, speech-dispatcher (+ speech-dispatcher-espeak-ng),
# python3-gi + gir1.2-atspi-2.0, xvfb, xdotool; openbox optional.
#   apt-get install orca at-spi2-core speech-dispatcher speech-dispatcher-espeak-ng \
#       python3-gi gir1.2-atspi-2.0 xvfb xdotool openbox
#
# Everything runs on a private Xvfb display inside a private D-Bus session, so
# the user's desktop, display and accessibility bus are never touched; speech
# goes to the ALSA null device (nothing is played). Orca's spoken output is
# taken from its debug log ("SPEECH OUTPUT:" lines).
#
#   TMPDIR=/path/to/scratch qa/accessibility/orca_acceptance.sh
#   RUSTPY_A11Y_PROFILE=release   use target/release/rustpy (default debug)
#
# Results: $TMPDIR/orca-*/report.json, orca-debug.out, atspi-tree.txt.
set -euo pipefail
cd "$(dirname "$0")/../.."
for tool in orca Xvfb xdotool dbus-run-session gdbus python3; do
    command -v "$tool" >/dev/null || { echo "missing $tool" >&2; exit 77; }
done
unset WAYLAND_DISPLAY DISPLAY PULSE_SERVER DBUS_SESSION_BUS_ADDRESS AT_SPI_BUS_ADDRESS
exec dbus-run-session -- python3 qa/accessibility/orca_acceptance.py "$@"
