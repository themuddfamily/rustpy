#!/usr/bin/env python3
"""Regenerate exact retained resource notices, or fail on drift with --check.

Reads only repository source files; does not resolve dependencies or run tools.
"""
import argparse
import importlib.util
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCES = [
    ('Locked Cargo dependencies — licenses, attribution and source locations',
     'packaging/CARGO-THIRD-PARTY-NOTICES.txt'),
    ('Vendored AccessKit AT-SPI common (MIT OR Apache-2.0)',
     'vendor/accesskit_atspi_common/LICENSE-MIT'),
    ('Vendored AccessKit AT-SPI common (alternative Apache license)',
     'vendor/accesskit_atspi_common/LICENSE-APACHE'),
    ('Vendored eframe (MIT OR Apache-2.0)',
     'vendor/eframe/LICENSE-MIT'),
    ('Vendored eframe (alternative Apache license)',
     'vendor/eframe/LICENSE-APACHE'),
    ('Vendored egui_glow (MIT OR Apache-2.0)',
     'vendor/egui_glow/LICENSE-MIT'),
    ('Vendored egui_glow (alternative Apache license)',
     'vendor/egui_glow/LICENSE-APACHE'),
    ('typeshed snapshot provenance',
     'crates/rustpy-semantic/typeshed/THIRD_PARTY_REVISION'),
    ('typeshed license',
     'crates/rustpy-semantic/typeshed/LICENSE'),
    ('pycodestyle 2.14.0 — https://github.com/PyCQA/pycodestyle/tree/2.14.0',
     'crates/rustpy-analysis/resources/pycodestyle/LICENSE'),
    ('coverage.py 7.6.1 license',
     'crates/rustpy-run/src/coverage_vendor/LICENSE.txt'),
    ('coverage.py 7.6.1 notice',
     'crates/rustpy-run/src/coverage_vendor/NOTICE.txt'),
    ('coverage.py runtime bundle provenance and modifications',
     'crates/rustpy-run/src/coverage_vendor/PROVENANCE.md'),
    ('tomli 2.0.1 license',
     'crates/rustpy-run/src/coverage_vendor/TOMLI-LICENSE'),
    ('Hack font license',
     'crates/rustpy-ui/assets/fonts/LICENSE-Hack.txt'),
    ('Noto Emoji font license',
     'crates/rustpy-ui/assets/fonts/OFL-NotoEmoji.txt'),
    ('Spelling vocabulary provenance and notice',
     'crates/rustpy-spell/NOTICE.md'),
    ('Solarized palette license',
     'crates/rustpy-plugins/examples/solarized-theme/LICENSE.solarized'),
    ('Solarized palette provenance notice',
     'crates/rustpy-plugins/examples/solarized-theme/NOTICE.solarized'),
    ('Embedded dependency resource provenance and font copyright notices',
     'packaging/NOTICE-dependency-resources.txt'),
    ('epaint_default_fonts 0.36.2 — SIL Open Font License',
     'packaging/LICENSE-epaint-OFL.txt'),
    ('epaint_default_fonts 0.36.2 — Ubuntu Font Licence',
     'packaging/LICENSE-epaint-Ubuntu.txt'),
    ('epaint_default_fonts 0.36.2 — Hack and Bitstream Vera notices',
     'packaging/LICENSE-epaint-Hack.txt'),
    ('epaint_default_fonts 0.36.2 — emoji-icon-font MIT license',
     'packaging/LICENSE-epaint-emoji-icon.txt'),
    ('webpki-roots 1.0.9 — CDLA-Permissive-2.0',
     'packaging/LICENSE-webpki-roots.txt'),
    ('borrow-or-share 0.2.4 — MIT No Attribution',
     'packaging/LICENSE-borrow-or-share.txt'),
    ('Native database client dependency additions',
     'packaging/NOTICE-native-database-dependencies.txt'),
    ('Embedded protobuf and TensorFlow attribution',
     'packaging/NOTICE-embedded-resource-supplement.txt'),
    ('Native library licenses and exceptions',
     'packaging/NOTICE-native-runtime-dependencies.txt'),
]
HEADER = """RustPy — third-party licenses, attribution and source locations
============================================

Generated from retained repository notices by packaging/update-notices.py.
This bundle preserves the texts below verbatim; headings identify their sources.
It covers every registry package in the application, QA and fuzz lockfiles,
the retained embedded resources, locally vendored GUI sources, and native
library notices. Inclusion is conservative: optional, build, development and
other-target dependencies are not necessarily present in every executable.
Artifact-specific runtime evidence and covered source accompany binaries in
Native-Licenses.zip. Read the retained build and replacement instructions there.
The Black test corpus and other source-only fixtures retain their own notices
and provenance beside their source. RustPy's separate code uses the project
LICENSE; third-party components retain their respective terms.
Externally installed debugpy, Python packages, database servers and vendor ODBC
drivers are not bundled merely by being supported. Cargo license policy checks
and exact artifact linkage/source evidence remain separate release checks.
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    spec = importlib.util.spec_from_file_location('cargo_notices', ROOT / 'scripts/generate-cargo-notices.py')
    cargo_notices = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(cargo_notices)
    try:
        cargo_notices.check()
    except (ValueError, OSError, KeyError) as error:
        parser.exit(1, f'Cargo license evidence missing/stale: {error}; run scripts/generate-cargo-notices.py\n')
    expected = HEADER + "".join(
        "\n\n" + title + "\nSource: " + path + "\n" + "-" * 72 + "\n"
        + (ROOT / path).read_text(encoding="utf-8")
        for title, path in SOURCES
    )
    output = ROOT / "THIRD-PARTY-NOTICES.txt"
    if args.check:
        if not output.is_file() or output.read_text(encoding="utf-8") != expected:
            parser.exit(1, "Bundled notices are missing/stale; run packaging/update-notices.py\n")
    else:
        output.write_text(expected, encoding="utf-8", newline="\n")


if __name__ == "__main__":
    main()
