# Fixed wrapper: all paths and provider arguments are positional, never shell source.
# Zenity and KDialog use status 1 for user cancellation. Other errors propagate
# to the supervised owner, which offers the built-in browser.
"$@"
chooser_status=$?
if [ "$chooser_status" -eq 1 ]; then
    printf '%s\n' RUSTPY_NATIVE_CANCELLED
    exit 0
fi
exit "$chooser_status"
