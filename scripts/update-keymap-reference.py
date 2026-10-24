#!/usr/bin/env python3
"""Generate the built-in keymap reference from declarative action metadata.
This reads Rust source only; it does not launch RustPy or resolve user keymaps.
"""
import re
from pathlib import Path
ROOT = Path(__file__).resolve().parent.parent
HEADER = """# Keymap reference

This table is generated from `crates/rustpy-ui/src/actions.rs`. It lists built-in
base bindings, including unbound actions. Settings › Keymap, the selected parent
map, plugins and user overrides determine the effective bindings. Find Action
shows the current binding; macOS and alternative parent maps can differ. Search
by action name or stable action ID when importing a keymap. Chords are dispatched
in their active context, and editor input/IME must not be treated as a global chord.

To change a binding, select an action in Settings › Keymap, add keyboard/mouse
shortcuts, and resolve displayed conflicts. Duplicate or export a map before
large changes. The bundled map assets live in `crates/rustpy-ui/assets/keymaps/`.
Regenerate this reference with `python3 scripts/update-keymap-reference.py` after
changing base action metadata.

| Action | Stable ID | Base shortcuts |
|---|---|---|
"""
def render(source):
    rows = []
    pattern = r'^\s*\w+\s*=>\s*\("([^"\\]*)",\s*"([^"\\]*)",\s*\[([^\]]*)\]\)'
    for identifier, label, shortcuts in re.findall(pattern, source, re.MULTILINE):
        keys = re.findall(r'"([^"\\]*)"', shortcuts)
        rows.append(f"| {label.replace('|', '&#124;')} | `{identifier}` | {'; '.join(keys) or 'Unbound'} |\n")
    if not rows:
        raise ValueError("no action metadata found; update generator for source schema")
    return HEADER + ''.join(rows)
if __name__ == '__main__':
    (ROOT / 'docs/keymap.md').write_text(render((ROOT / 'crates/rustpy-ui/src/actions.rs').read_text()), encoding='utf-8')
