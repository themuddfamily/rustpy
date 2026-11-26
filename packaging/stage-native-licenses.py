#!/usr/bin/env python3
"""Stage exact native and covered Cargo sources into a distributable ZIP.

Run after copying runtime libraries and before creating the final artifact.
RUSTPY_NATIVE_RUNTIME_MANIFEST supplies reviewed runtime component evidence.
The ZIP is published next to the packages, not inside them, so staging also
checks that every licence and notice text it retains is in the shipped
THIRD-PARTY-NOTICES.txt.
"""
import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parent.parent
STORED_SUFFIXES = ('.crate', '.gz', '.tgz', '.xz', '.bz2', '.zst', '.zip', '.nupkg', '.docx')
NOTICES = ROOT / 'THIRD-PARTY-NOTICES.txt'
NOTICE_NAMES = ('license', 'licence', 'copying', 'copyright', 'notice')
# Vendor licence agreements kept as documents: the notices must name the component instead.
DOCUMENT_SUFFIXES = ('.docx', '.rtf', '.html', '.htm', '.pdf')
REGENERATE = ('add the missing texts to the notices (packaging/update-notices.py SOURCES; '
              'for the Rust runtime: stage-native-licenses.py --rust-runtime-notice) and '
              'run packaging/update-notices.py')


def normalise(text):
    return re.sub(r'\s+', ' ', text.replace('\ufeff', '')).strip()


def read_text(path):
    data = path.read_bytes()
    try:
        text = data.decode('utf-8')
    except UnicodeDecodeError:
        text = data.decode('cp1252', errors='replace')  # bundles written before UTF-8 was explicit
    return text.replace('\r\n', '\n')


def notice_texts(text):
    """Split a retained notice into its licence texts (dropping Source/SHA256 headers)."""
    if text.startswith('Locked Cargo package:'):
        text = text.split('\n', 2)[2] if text.count('\n') >= 2 else ''
    parts = re.split(r'\nSource: [^\n]*\n(?:SHA256: [0-9a-f]{64}\n)?', '\n' + text)
    return [part for part in parts if part.strip()]


def rust_source_texts(archive):
    """Licence files of the rust-src component: the standard library and its vendored crates."""
    texts = []
    with tarfile.open(archive) as source:
        for member in source:
            if member.isfile() and Path(member.name).name.lower().startswith(NOTICE_NAMES):
                prefix = '/rust-src/lib/rustlib/src/rust/'
                path = member.name.split(prefix, 1)[1] if prefix in member.name else member.name.split('/', 1)[-1]
                texts.append((path, source.extractfile(member).read().decode('utf-8', errors='replace')))
    return texts


def uncovered_notices(bundle, notices):
    """Licence texts retained in a staged bundle that the shipped notices lack."""
    shipped = normalise(notices)
    missing = []
    runtime_notice = bundle / 'NOTICE-native-runtime-dependencies.txt'
    if runtime_notice.is_file() and normalise(read_text(runtime_notice)) not in shipped:
        missing.append('native crate notices differ from packaging/NOTICE-native-runtime-dependencies.txt '
                       '(regenerate it with scripts/collect-native-licenses.py --notice-only)')
    manifest = json.loads((bundle / 'runtime-manifest.json').read_text(encoding='utf-8'))
    for record in manifest.get('components', []):
        label = f"{record.get('name')} {record.get('version')}"
        retained = {}
        for entry in record.get('retained_evidence', []):
            relative = entry['path'].replace('\\', '/')  # recorded with the build host's separator
            name = relative.rsplit('/', 1)[-1]
            retained[name.split('-', 1)[1] if '-' in name else name] = bundle / relative
        sources = record.get('source_archives') or {}
        if any(name.startswith('rust-src-') for name in sources):
            for name in sources:
                if name.startswith('rust-src-'):
                    for path, text in rust_source_texts(retained[name]):
                        if normalise(text) not in shipped:
                            missing.append(f'{label}: {path}')
            if any(name.startswith('rustc-') for name in sources):
                compiler_rt = [path for name, path in retained.items() if name.endswith('-compiler-rt-LICENSE.TXT')]
                if not compiler_rt:
                    missing.append(f'{label}: the compiler-rt licence was not retained')
                for path in compiler_rt:
                    if normalise(read_text(path)) not in shipped:
                        missing.append(f'{label}: compiler-rt LICENSE.TXT')
            continue
        for name in record.get('license_files', []):
            path = retained.get(name)
            if path is None:
                missing.append(f'{label}: {name} was not retained')
            elif path.suffix.lower() in DOCUMENT_SUFFIXES:
                if record.get('name') not in notices:
                    missing.append(f'{label}: the notices do not name this component ({name})')
            else:
                for text in notice_texts(read_text(path)):
                    if normalise(text) not in shipped:
                        missing.append(f'{label}: {name}: {normalise(text)[:60]}')
    return missing


def rust_runtime_notice(rust_source, compiler_rt_source=None):
    """Render packaging/NOTICE-rust-runtime.txt from the official Rust source archives."""
    texts = {}
    for path, text in rust_source_texts(rust_source):
        texts.setdefault(text, []).append(path)
    if compiler_rt_source:
        with tarfile.open(compiler_rt_source) as source:
            for member in source:
                if member.isfile() and member.name.endswith('/src/llvm-project/compiler-rt/LICENSE.TXT'):
                    text = source.extractfile(member).read().decode('utf-8', errors='replace')
                    texts.setdefault(text, []).append('src/llvm-project/compiler-rt/LICENSE.TXT')
                    break
            else:
                raise ValueError('The Rust source archive has no compiler-rt licence')
    version = re.search(r'rust-src-([0-9][^/]*?)\.tar', Path(rust_source).name)
    rendered = ['Rust standard library and compiler runtime\n',
                f"Licence files of the official Rust {version[1] if version else ''} rust-src component (the standard\n"
                'library and the crates it vendors, statically linked into every RustPy executable)\n'
                'and of the compiler-rt runtime built into compiler_builtins. Generated by\n'
                'packaging/stage-native-licenses.py --rust-runtime-notice; identical texts appear once.\n']
    for text, paths in sorted(texts.items(), key=lambda item: sorted(item[1])[0]):
        rendered.append('\nFiles: ' + ', '.join(sorted(paths)) + '\n' + '-' * 72 + '\n'
                        + text + ('' if text.endswith('\n') else '\n'))
    return ''.join(rendered)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    runtime = parser.add_mutually_exclusive_group(required=True)
    runtime.add_argument('--runtime-root', type=Path)
    runtime.add_argument('--runtime-file', type=Path)
    runtime.add_argument('--check-existing', type=Path, metavar='ZIP',
                         help='Verify that a bundle staged earlier in this build matches --binary, without collecting again')
    runtime.add_argument('--rust-runtime-notice', type=Path, metavar='OUT',
                         help='Write the Rust runtime notice from --rust-source (and --compiler-rt-source)')
    parser.add_argument('--rust-source', type=Path, help='Official rust-src-<version>.tar.xz')
    parser.add_argument('--compiler-rt-source', type=Path, help='Official rustc-<version>-src.tar.xz')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--source-revision', help='With --check-existing: required source revision of the build')
    parser.add_argument('--target', help='With --check-existing: required Rust target of the build')
    parser.add_argument('--binary', type=Path, help='Exact distributed executable; defaults to --runtime-file')
    parser.add_argument('--binary-stage', choices=['packaged', 'pre-sign'], default='packaged')
    parser.add_argument('--required-native-file', action='append', default=[],
                        help='Artifact-relative runtime path that must be declared in manifest components')
    args = parser.parse_args()
    if args.rust_runtime_notice:
        if not args.rust_source:
            parser.error('--rust-runtime-notice requires --rust-source')
        args.rust_runtime_notice.write_text(rust_runtime_notice(args.rust_source, args.compiler_rt_source),
                                            encoding='utf-8', newline='\n')
        return
    if args.check_existing:
        if not args.binary:
            parser.error('--check-existing requires --binary')
        try:
            check_existing(args.check_existing, args.binary, args.source_revision, args.target)
        except (ValueError, OSError, KeyError, zipfile.BadZipFile) as error:
            parser.exit(1, f'Licence evidence bundle check failed: {error}\n')
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
        missing = uncovered_notices(bundle, NOTICES.read_text(encoding='utf-8'))
        if missing:
            parser.exit(1, 'THIRD-PARTY-NOTICES.txt lacks licence texts that the packages must carry:\n  '
                        + '\n  '.join(missing[:40]) + f'\n({len(missing)} in all) To fix: {REGENERATE}\n')
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
