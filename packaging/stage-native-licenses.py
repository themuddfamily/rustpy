#!/usr/bin/env python3
"""Stage exact native and covered Cargo sources into a distributable ZIP.

Run after copying runtime libraries and before creating the final artifact.
RUSTPY_NATIVE_RUNTIME_MANIFEST supplies reviewed runtime component evidence.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parent.parent
STORED_SUFFIXES = ('.crate', '.gz', '.tgz', '.xz', '.bz2', '.zst', '.zip', '.nupkg', '.docx')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    runtime = parser.add_mutually_exclusive_group(required=True)
    runtime.add_argument('--runtime-root', type=Path)
    runtime.add_argument('--runtime-file', type=Path)
    runtime.add_argument('--check-existing', type=Path, metavar='ZIP',
                         help='Verify that a bundle staged earlier in this build matches --binary, without collecting again')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--source-revision', help='With --check-existing: required source revision of the build')
    parser.add_argument('--target', help='With --check-existing: required Rust target of the build')
    parser.add_argument('--binary', type=Path, help='Exact distributed executable; defaults to --runtime-file')
    parser.add_argument('--binary-stage', choices=['packaged', 'pre-sign'], default='packaged')
    parser.add_argument('--required-native-file', action='append', default=[],
                        help='Artifact-relative runtime path that must be declared in manifest components')
    args = parser.parse_args()
    if args.check_existing:
        if not args.binary:
            parser.error('--check-existing requires --binary')
        try:
            check_existing(args.check_existing, args.binary, args.source_revision, args.target)
        except (ValueError, OSError, KeyError, zipfile.BadZipFile) as error:
            parser.exit(1, f'Native-Licenses.zip check failed: {error}\n')
        return
    if not args.output:
        parser.error('--output is required')
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
            with zipfile.ZipFile(staged, 'w', compression=zipfile.ZIP_DEFLATED, allowZip64=True) as archive:
                for source in sorted(bundle.rglob('*')):
                    if source.is_file():
                        entry = zipfile.ZipInfo(source.relative_to(bundle.parent).as_posix(), (1980, 1, 1, 0, 0, 0))
                        entry.compress_type = compression_for(source)
                        entry.external_attr = 0o100644 << 16
                        entry.file_size = source.stat().st_size
                        with source.open('rb') as data, archive.open(entry, 'w') as member:
                            shutil.copyfileobj(data, member, 1 << 20)
            staged.replace(output)
        finally:
            staged.unlink(missing_ok=True)


def check_existing(archive_path, binary, source_revision=None, target=None):
    """Confirm a bundle staged earlier describes exactly this executable.

    Packagers that share one staged bundle call this instead of collecting again:
    the full collection gate already ran for the same binary when it was staged.
    With source_revision/target, the bundle's retained link build records must also
    name this checkout and target (for a binary restored from an earlier build).
    """
    with binary.open('rb') as data:
        actual = hashlib.file_digest(data, 'sha256').hexdigest()
    with archive_path.open('rb') as source, zipfile.ZipFile(source) as archive:
        names = set(archive.namelist())
        required = {'Native-Licenses/COMPLETE', 'Native-Licenses/runtime-manifest.json',
                    'Native-Licenses/cargo-covered-sources.tar.gz'}
        if not required <= names:
            raise ValueError('staged bundle is incomplete: ' + ', '.join(sorted(required - names)))
        manifest = json.loads(archive.read('Native-Licenses/runtime-manifest.json'))
        if manifest.get('binary_sha256') != actual:
            raise ValueError(f'staged bundle describes a different executable than {binary}')
        if source_revision is None and target is None:
            return
        records = [json.loads(archive.read(name)) for name in sorted(names)
                   if name.startswith('Native-Licenses/') and name.endswith('build-record.json')]
    if not records:
        raise ValueError('staged bundle has no link build record to identify its source')
    for record in records:
        toolchain = record.get('toolchain', {})
        if record.get('link', {}).get('binary_sha256') != actual:
            raise ValueError('link build record describes a different executable')
        if source_revision is not None and toolchain.get('source_revision') != source_revision:
            raise ValueError(f'executable was built from {toolchain.get("source_revision")}, not {source_revision}')
        if target is not None and toolchain.get('target') != target:
            raise ValueError(f'executable was built for {toolchain.get("target")}, not {target}')


def compression_for(path):
    # Source archives and documents are already compressed; deflating them again
    # costs time in every packager and saves nothing.
    return zipfile.ZIP_STORED if path.name.lower().endswith(STORED_SUFFIXES) else zipfile.ZIP_DEFLATED


if __name__ == '__main__':
    main()
