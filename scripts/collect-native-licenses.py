#!/usr/bin/env python3
"""Retain locked native sources and fail closed on uncovered redistributed libraries.

Runtime evidence JSON schema (paths to evidence relative to the JSON file):
{"binary_sha256": "<sha256 of actual packaged RustPy executable>",
 "system_static_components": [],
 "components": [{"name": "unixODBC", "version": "2.3.12",
 "files": {"lib/libodbc.so.2.0.0": "<sha256 of shipped library>"},
 "license_files": ["copyright"],
 "source_archives": {"unixODBC-2.3.12.tar.gz": "<sha256>"},
 "build_instructions": "build.txt", "replacement_instructions": "replace.txt"}]}

system_static_components must explicitly list system/toolchain native code
statically linked into the executable (same evidence fields except files),
or be explicitly [] when there is none. Native code already covered by the
locked native crate archives need not be duplicated in that list. The manifest
binds the producer declaration and evidence to --binary by SHA256.

Evidence must be prepared for the actual binaries, including distribution patches
and build scripts. All runtime components conservatively require corresponding
source and build/replacement instructions. URLs and unversioned license labels
alone are deliberately insufficient. This validates evidence integrity/coverage;
it does not infer legal compatibility from a declared SPDX expression.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import tarfile
import tomllib

NATIVE = {'libgit2-sys', 'libssh2-sys', 'libz-sys', 'libsqlite3-sys', 'openssl-src'}
ROOT = Path(__file__).resolve().parents[1]


def digest(path):
    with path.open('rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()


def licenses(source):
    return sorted(p for p in source.rglob('*') if p.is_file() and
                  p.name.lower().startswith(('license', 'copying', 'copyright', 'notice')))


def locked_native(lock, home, vendor=None):
    for package in tomllib.loads(lock.read_text())['package']:
        if package['name'] not in NATIVE:
            continue
        ident = package['name'] + '-' + package['version']
        matches = list((home / 'registry/src').glob('*/' + ident))
        archives = list((home / 'registry/cache').glob('*/' + ident + '.crate'))
        if vendor:
            extra_cache = os.environ.get('RUSTPY_NATIVE_CRATE_CACHE')
            if extra_cache:
                archives += list(Path(extra_cache).rglob(ident + '.crate'))
            archives = list(dict.fromkeys(archives))
            if len(archives) != 1 or digest(archives[0]) != package['checksum']:
                raise ValueError(f'{ident}: vendor verification needs checksum-verified original .crate; provide RUSTPY_NATIVE_CRATE_CACHE')
            authenticated = {}
            with tarfile.open(archives[0]) as tf:
                for member in tf.getmembers():
                    if member.isfile():
                        rel = member.name.split('/', 1)[1]
                        authenticated[rel] = hashlib.sha256(tf.extractfile(member).read()).hexdigest()
            candidates = [d for d in vendor.iterdir() if d.is_dir() and (d / 'Cargo.toml').is_file() and tomllib.loads((d / 'Cargo.toml').read_text()).get('package', {}).get('name') == package['name'] and tomllib.loads((d / 'Cargo.toml').read_text()).get('package', {}).get('version') == package['version']]
            if len(candidates) != 1:
                raise ValueError(f'{ident}: exact vendor source missing/ambiguous')
            source = candidates[0]
            checks = json.loads((source / '.cargo-checksum.json').read_text())
            if checks.get('package') != package['checksum']:
                raise ValueError(f'{ident}: vendor package checksum differs from lock')
            if checks['files'] != authenticated:
                raise ValueError(f'{ident}: vendor checksum inventory differs from locked original archive')
            for name, sha in authenticated.items():
                checked_file(source, name, sha)
            actual = {p.relative_to(source).as_posix() for p in source.rglob('*') if p.is_file() and p.name != '.cargo-checksum.json'}
            if actual != set(checks['files']):
                raise ValueError(f'{ident}: vendor file inventory differs from authenticated manifest')
            yield package, source
            continue
        if len(matches) != 1 or len(archives) != 1:
            raise ValueError(f'{ident}: exact source/cache missing; run cargo fetch --locked')
        archive = archives[0]
        if digest(archive) != package['checksum']:
            raise ValueError(f'{ident}: cached source archive differs from Cargo.lock')
        # Read licenses from the authenticated archive, never a possibly edited
        # extracted registry tree. Reject traversal and symlinks on extraction.
        yield package, archive


def archive_notices(package, archive):
    parts = []
    if archive.is_dir():
        for path in licenses(archive):
            data = path.read_bytes()
            parts.append(f'\nSource: {path.relative_to(archive)}\nSHA256: {hashlib.sha256(data).hexdigest()}\n\n' + data.decode('utf-8', errors='replace'))
        if package['name'] == 'libsqlite3-sys':
            path = archive / 'sqlite3/sqlite3.c'
            parts.append('\nSource: sqlite3/sqlite3.c (first 45 lines)\n' + '\n'.join(path.read_text().splitlines()[:45]))
        if not parts:
            raise ValueError(f'{archive}: no license texts')
        return '\n'.join(parts)
    with tarfile.open(archive) as tf:
        for member in sorted(tf.getmembers(), key=lambda m: m.name):
            if not member.isfile():
                continue
            name = Path(member.name).name.lower()
            if not name.startswith(('license', 'copying', 'copyright', 'notice')):
                continue
            data = tf.extractfile(member).read()
            parts.append(f'\nSource: {member.name}\nSHA256: {hashlib.sha256(data).hexdigest()}\n\n' + data.decode('utf-8', errors='replace'))
        if package['name'] == 'libsqlite3-sys':
            # sqlite3.c explicitly records its public-domain dedication.
            for member in tf.getmembers():
                if member.name.endswith('/sqlite3/sqlite3.c'):
                    data = tf.extractfile(member).read()
                    parts.append('\nSource: ' + member.name + ' (first 45 lines)\n' + '\n'.join(data.decode('utf-8', errors='replace').splitlines()[:45]))
    if not parts:
        raise ValueError(f'{archive}: no license texts')
    return '\n'.join(parts)


def checked_file(base, name, expected=None):
    path = (base / name).resolve()
    if not path.is_file() or path.stat().st_size == 0:
        raise ValueError(f'missing/empty evidence: {path}')
    if expected is not None and (not re.fullmatch('[0-9a-f]{64}', expected) or digest(path) != expected):
        raise ValueError(f'SHA256 mismatch: {path}')
    return path


def native_binary(path):
    """Recognize executable/library file formats without invoking host tools."""
    with path.open('rb') as source:
        header = source.read(64)
        magic = header[:4]
        if magic == b'\x7fELF':
            return True
        if magic in (b'\xfe\xed\xfa\xce', b'\xce\xfa\xed\xfe',
                     b'\xfe\xed\xfa\xcf', b'\xcf\xfa\xed\xfe'):
            return True
        # Universal Mach-O also shares CAFEBABE with Java class files. A real
        # universal header has a small architecture count, not class versions.
        if magic in (b'\xca\xfe\xba\xbe', b'\xca\xfe\xba\xbf',
                     b'\xbe\xba\xfe\xca', b'\xbf\xba\xfe\xca'):
            endian = 'big' if magic[:1] == b'\xca' else 'little'
            if len(header) >= 8 and 0 < int.from_bytes(header[4:8], endian) <= 64:
                return True
        if header[:2] == b'MZ' and len(header) == 64:
            source.seek(int.from_bytes(header[60:64], 'little'))
            return source.read(4) == b'PE\0\0'
    return False


def collect_runtime(manifest, runtime, output, binary, binary_stage):
    if not manifest:
        raise ValueError('--runtime-manifest is required, including explicit static linkage declaration')
    evidence = json.loads(manifest.read_text())
    if not isinstance(evidence.get('binary_sha256'), str) or not re.fullmatch('[0-9a-f]{64}', evidence['binary_sha256']):
        raise ValueError('binary_sha256 must be a SHA256 hex digest of the packaged executable')
    checked_file(binary.parent, binary.name, evidence['binary_sha256'])
    static = evidence.get('system_static_components')
    if not isinstance(static, list):
        raise ValueError('system_static_components must be an explicit list (empty only when no system/toolchain native code is statically linked)')
    if not isinstance(evidence.get('components'), list):
        raise ValueError('components must be an explicit runtime library list')
    covered = {}
    records = []
    combined = [('runtime', item) for item in evidence['components']] + [('static', item) for item in static]
    for index, (kind, item) in enumerate(combined):
        if not item.get('name') or not item.get('version'):
            raise ValueError('native component needs name and exact version')
        if kind == 'runtime' and not item.get('files'):
            raise ValueError('runtime component needs files')
        dest = output / kind / str(index)
        dest.mkdir(parents=True)
        for rel, sha in (item['files'] if kind == 'runtime' else {}).items():
            if not isinstance(sha, str) or not re.fullmatch('[0-9a-f]{64}', sha):
                raise ValueError(f'invalid runtime SHA256: {rel}')
            path = (runtime / rel).resolve()
            if not path.is_relative_to(runtime.resolve()):
                raise ValueError(f'runtime path escapes staging root: {rel}')
            checked_file(runtime, rel, sha)
            if rel in covered:
                raise ValueError(f'duplicate runtime evidence: {rel}')
            covered[rel] = item['name']
        if not item.get('license_files') or not item.get('source_archives'):
            raise ValueError(f'{item["name"]}: license files AND exact corresponding source archives required')
        files = [(name, None) for name in item['license_files']]
        for name, sha in item['source_archives'].items():
            if not isinstance(sha, str) or not re.fullmatch('[0-9a-f]{64}', sha):
                raise ValueError(f'invalid source SHA256: {name}')
        files += list(item['source_archives'].items())
        for key in ('build_instructions', 'replacement_instructions'):
            if not item.get(key):
                raise ValueError(f'{item["name"]}: {key} required')
            files.append((item[key], None))
        copied = []
        for count, (name, sha) in enumerate(files):
            source = checked_file(manifest.parent, name, sha)
            # Prefix retains evidence even when separate packages share basename.
            target = dest / (str(count) + '-' + source.name)
            shutil.copyfile(source, target)
            copied.append({'path': str(target.relative_to(output)), 'sha256': digest(target)})
        records.append({**item, 'linkage': kind, 'retained_evidence': copied})
    for path in runtime.rglob('*'):
        if not path.is_file() or path.resolve().is_relative_to(output.resolve()) or path.resolve() == binary.resolve():
            continue
        if native_binary(path) or re.search(r'(\.dll$|\.dylib$|\.so(?:\.|$))', path.name, re.I):
            rel = path.relative_to(runtime).as_posix()
            if rel not in covered:
                raise ValueError(f'uncovered redistributed native binary: {rel}; supply --runtime-manifest')
    (output / 'runtime-manifest.json').write_text(json.dumps({'binary_sha256': evidence['binary_sha256'], 'binary_stage': binary_stage, 'components': records, 'system_static_components': static}, indent=2) + '\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--cargo-lock', type=Path, default=ROOT / 'Cargo.lock')
    parser.add_argument('--cargo-home', type=Path, default=Path(os.environ.get('CARGO_HOME', Path.home() / '.cargo')))
    parser.add_argument('--cargo-vendor', type=Path, default=os.environ.get('RUSTPY_CARGO_VENDOR_DIR') or ((ROOT / 'cargo/vendor') if (ROOT / 'cargo/vendor').is_dir() else None), help='Cargo vendor tree with intact .cargo-checksum.json manifests')
    parser.add_argument('--binary', type=Path, help='actual packaged RustPy executable; required except --notice-only')
    parser.add_argument('--binary-stage', choices=('packaged', 'pre-sign'), default='packaged', help='hash stage: packaged binary (default), or macOS pre-sign binary; final artifact checksums remain separate')
    parser.add_argument('--runtime-root', type=Path)
    parser.add_argument('--runtime-manifest', type=Path, default=os.environ.get('RUSTPY_NATIVE_RUNTIME_MANIFEST'))
    parser.add_argument('--notice-only', type=Path, help='regenerate checked-in notice without exporting source or scanning runtimes')
    args = parser.parse_args()
    try:
        packages = list(locked_native(args.cargo_lock, args.cargo_home, args.cargo_vendor))
        notice = 'RustPy — native library source notices\n=====================================\n\nThese are exact native license files inside checksum-verified locked crate\narchives, including nested upstream licenses, exceptions and notices. Inclusion\nis conservative, not evidence that every native option is enabled. System and\nredistributed runtime libraries require separate artifact-specific evidence.\n\n'
        records = []
        for package, archive in packages:
            notice += '\n\n' + package['name'] + ' ' + package['version'] + '\n' + archive_notices(package, archive)
            records.append({'name': package['name'], 'version': package['version'], 'sha256': package['checksum'], 'source_url': f'https://static.crates.io/crates/{package["name"]}/{package["name"]}-{package["version"]}.crate'})
        if args.notice_only:
            args.notice_only.write_text(notice)
            return
        if not args.output or not args.runtime_root or not args.binary:
            raise ValueError('--output, --runtime-root and --binary are required for a distribution gate')
        output = args.output.resolve()
        if output.exists() and any(output.iterdir()):
            raise ValueError(f'output must be empty: {output}')
        output.mkdir(parents=True, exist_ok=True)
        (output / 'sources').mkdir()
        for package, archive in packages:
            if archive.is_dir():
                with tarfile.open(output / 'sources' / (archive.name + '.tar.gz'), 'w:gz') as tf:
                    tf.add(archive, arcname=archive.name)
            else:
                shutil.copyfile(archive, output / 'sources' / archive.name)
        (output / 'NOTICE-native-runtime-dependencies.txt').write_text(notice)
        (output / 'native-crates.json').write_text(json.dumps(records, indent=2) + '\n')
        collect_runtime(args.runtime_manifest, args.runtime_root, output, args.binary, args.binary_stage)
        (output / 'COMPLETE').write_text('Native source and runtime evidence collection completed.\n')
    except (ValueError, KeyError, OSError, tarfile.TarError) as error:
        parser.exit(1, 'native license gate: ' + str(error) + '\n')


if __name__ == '__main__':
    main()
