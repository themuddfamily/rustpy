#!/usr/bin/env python3
"""Stage exact native and covered Cargo sources into a distributable ZIP.

Run after copying runtime libraries and before creating the final artifact.
RUSTPY_NATIVE_RUNTIME_MANIFEST supplies reviewed runtime component evidence.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    runtime = parser.add_mutually_exclusive_group(required=True)
    runtime.add_argument('--runtime-root', type=Path)
    runtime.add_argument('--runtime-file', type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--binary', type=Path, help='Exact distributed executable; defaults to --runtime-file')
    parser.add_argument('--binary-stage', choices=['packaged', 'pre-sign'], default='packaged')
    parser.add_argument('--required-native-file', action='append', default=[],
                        help='Artifact-relative runtime path that must be declared in manifest components')
    args = parser.parse_args()
    binary = args.binary or args.runtime_file
    if binary is None:
        parser.error('--binary is required with --runtime-root')
    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    # Work outside the runtime root so collectors never mistake license sources
    # for additional distributed runtime libraries.
    temporary_root = Path(os.environ.get('TMPDIR', Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')) / 'license-staging'))
    temporary_root.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='native-', dir=temporary_root) as temporary:
        bundle = Path(temporary) / 'Native-Licenses'
        runtime_root = args.runtime_root
        if args.runtime_file:
            runtime_root = Path(temporary) / 'runtime'
            runtime_root.mkdir()
            shutil.copy2(args.runtime_file, runtime_root / args.runtime_file.name)
            binary = runtime_root / args.runtime_file.name
        command = [sys.executable, str(ROOT / 'scripts/collect-native-licenses.py'),
                   '--output', str(bundle), '--runtime-root', str(runtime_root.resolve()),
                   '--binary', str(binary.resolve()), '--binary-stage', args.binary_stage]
        manifest = os.environ.get('RUSTPY_NATIVE_RUNTIME_MANIFEST')
        if args.required_native_file:
            if not manifest:
                parser.error('--required-native-file requires RUSTPY_NATIVE_RUNTIME_MANIFEST')
            evidence = json.loads(Path(manifest).read_text(encoding='utf-8'))
            covered = {name for component in evidence.get('components', []) for name in component.get('files', [])}
            missing = set(args.required_native_file) - covered
            if missing:
                parser.error('Runtime manifest does not cover required native files: ' + ', '.join(sorted(missing)))
        if manifest:
            command += ['--runtime-manifest', str(Path(manifest).resolve())]
        vendor = os.environ.get('RUSTPY_CARGO_VENDOR_DIR')
        if vendor:
            command += ['--cargo-vendor', str(Path(vendor).resolve())]
        subprocess.run(command, cwd=ROOT, check=True)
        cargo_command = [sys.executable, str(ROOT / 'scripts/generate-cargo-notices.py'),
                         '--check', '--source-bundle', str(bundle / 'cargo-covered-sources.tar.gz')]
        if vendor:
            cargo_command += ['--cargo-vendor', str(Path(vendor).resolve())]
        subprocess.run(cargo_command, cwd=ROOT, check=True)
        staged = output.with_suffix(output.suffix + '.part')
        try:
            with zipfile.ZipFile(staged, 'w', compression=zipfile.ZIP_DEFLATED) as archive:
                for source in sorted(bundle.rglob('*')):
                    if source.is_file():
                        entry = zipfile.ZipInfo(source.relative_to(bundle.parent).as_posix(), (1980, 1, 1, 0, 0, 0))
                        entry.compress_type = zipfile.ZIP_DEFLATED
                        entry.external_attr = 0o100644 << 16
                        archive.writestr(entry, source.read_bytes())
            staged.replace(output)
        finally:
            staged.unlink(missing_ok=True)


if __name__ == '__main__':
    main()
