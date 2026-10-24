#!/usr/bin/env python3
"""Fetch primary CPython fixture data, extract only Lib/*.py, build independent AST oracles."""
import argparse, shutil, ast, gzip, hashlib, json, os, pathlib, sys, tarfile, urllib.request, warnings
P = pathlib.Path
ARCHIVE_SHA256 = '56bfef1fdfc1221ce6720e43a661e3eb41785dd914ce99698d8c7896af4bdaa1'
URL = 'https://www.python.org/ftp/python/3.12.3/Python-3.12.3.tar.xz'
DOWNLOAD_LIMIT = 32 * 1024 * 1024
DISK_LIMIT = 100 * 1024 * 1024
SOURCE_LIMIT = 16 * 1024 * 1024

def sha(path):
    with path.open('rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()

def normalize(v):
    if isinstance(v, ast.AST):
        return {'node': type(v).__name__, 'fields': {k: normalize(getattr(v, k)) for k in v._fields}}
    if isinstance(v, list):
        return [normalize(i) for i in v]
    if isinstance(v, (str, int, bool)) or v is None:
        return v
    if isinstance(v, (bytes, complex, float)) or v is Ellipsis:
        return {'literal_type': type(v).__name__, 'repr': repr(v)}
    raise TypeError(type(v).__name__)

def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--output', type=P, required=True)
    ap.add_argument('--archive', type=P, help='Existing official release archive; otherwise download the pinned URL')
    args = ap.parse_args()
    OUT = args.output.resolve()
    assert not sys.flags.optimize, 'Run without -O: validation assertions are required'
    assert sys.implementation.name == 'cpython' and sys.version_info[:3] == (3, 12, 3)
    assert sys.get_int_max_str_digits() == 4300, 'CPython default decimal admission limit required'
    OUT.mkdir(mode=0o700, exist_ok=False)
    archive = OUT / 'Python-3.12.3.tar.xz'
    if args.archive:
        original = args.archive.resolve(strict=True)
        assert sha(original) == ARCHIVE_SHA256 and original.stat().st_size <= DOWNLOAD_LIMIT
        with original.open('rb') as source, archive.open('xb') as destination:
            shutil.copyfileobj(source, destination, 64 * 1024)
            destination.flush(); os.fsync(destination.fileno())
        download = archive.stat().st_size
    else:
        with urllib.request.urlopen(URL, timeout=30) as response, archive.open('xb') as f:
            assert response.status == 200 and response.url == URL
            download = 0
            while chunk := response.read(64 * 1024):
                download += len(chunk)
                assert download <= DOWNLOAD_LIMIT
                f.write(chunk)
            f.flush(); os.fsync(f.fileno())
    assert sha(archive) == ARCHIVE_SHA256
    files, extracted = [], 0
    root = OUT / 'Lib'
    with tarfile.open(archive, 'r:xz') as tar:
        version = tar.extractfile('Python-3.12.3/Include/patchlevel.h').read().decode('ascii')
        assert '#define PY_VERSION              "3.12.3"' in version
        entries = sorted((m for m in tar.getmembers() if m.name.startswith('Python-3.12.3/Lib/') and m.name.endswith('.py')), key=lambda m: m.name)
        assert len(entries) == 1774
        seen = set()
        for m in entries:
            assert m.isfile() and m.size <= SOURCE_LIMIT
            relative = pathlib.PurePosixPath(m.name).relative_to('Python-3.12.3/Lib')
            assert not relative.is_absolute() and '..' not in relative.parts and str(relative) not in seen
            seen.add(str(relative))
            extracted += m.size
            assert extracted + download <= DISK_LIMIT
            destination = root.joinpath(*relative.parts)
            destination.parent.mkdir(parents=True, exist_ok=True)
            data = tar.extractfile(m).read(SOURCE_LIMIT + 1)
            assert len(data) == m.size
            with destination.open('xb') as f:
                f.write(data)
            files.append({'path': str(relative), 'source_sha256': hashlib.sha256(data).hexdigest(), 'bytes': len(data)})
    accepted = rejected = uncompressed = warning_count = 0
    rejected_files = []
    kinds = {}
    oracle_path = OUT / 'oracles.jsonl.gz'
    with oracle_path.open('xb') as raw:
        with gzip.GzipFile(filename='', fileobj=raw, mode='wb', mtime=0) as stream:
            for item in files:
                path = root / item['path']
                data = path.read_bytes()
                assert hashlib.sha256(data).hexdigest() == item['source_sha256']
                with warnings.catch_warnings(record=True) as emitted:
                    warnings.simplefilter('always')
                    try:
                        tree = ast.parse(data, filename=item['path'], mode='exec', type_comments=False)
                        for node in ast.walk(tree):
                            kinds[type(node).__name__] = kinds.get(type(node).__name__, 0) + 1
                        oracle = {'accepted': True, 'ast': normalize(tree)}
                        accepted += 1
                    except (SyntaxError, UnicodeError, ValueError) as e:
                        oracle = {'accepted': False, 'error_type': type(e).__name__}
                        rejected_files.append({'path': item['path'], 'error_type': type(e).__name__})
                        rejected += 1
                    warning_count += len(emitted)
                line = (json.dumps({'source': item, 'oracle': oracle}, ensure_ascii=True, separators=(',', ':')) + '\n').encode()
                uncompressed += len(line)
                stream.write(line)
                stream.flush()
                assert download + extracted + raw.tell() <= DISK_LIMIT
                assert sha(path) == item['source_sha256']
        raw.flush(); os.fsync(raw.fileno())
    assert (len(files), sum(f['path'].startswith('test/') for f in files), accepted, rejected) == (1774, 1019, 1765, 9)
    manifest = {'schema': 1, 'primary_url': URL, 'release_version': '3.12.3', 'archive_sha256': sha(archive), 'archive_bytes': download, 'corpus_root': str(root), 'stdlib_root': str(root), 'python': sys.version, 'interpreter_sha256': sha(P(sys.executable)), 'files': files, 'module_count': len(files), 'test_module_count': sum(f['path'].startswith('test/') for f in files), 'accepted_count': accepted, 'rejected_count': rejected, 'rejected_files': rejected_files, 'warnings_count': warning_count, 'ast_node_kinds': kinds, 'oracle_sha256': sha(oracle_path), 'oracle_uncompressed_bytes': uncompressed, 'oracle_compressed_bytes': oracle_path.stat().st_size, 'source_bytes': extracted, 'complete_release_lib_py': True, 'completeness_policy': 'Every regular .py member under official release Lib/, including test and intentionally invalid fixtures; no .py exclusion. Non-Python assets are not extracted.', 'normalization': 'Recursive ast._fields; omit _attributes positions; exec/type_comments=False; bytes/complex/float/Ellipsis literal_type+CPython repr', 'rust_execution': False, 'installation': 'Independent installed CPython3.12.3 parses complete upstream3.12.3 fixture bytes; no corpus module imported/executed.'}
    with (OUT / 'manifest.json').open('x') as f:
        json.dump(manifest, f, indent=2); f.write('\n'); f.flush(); os.fsync(f.fileno())
    print(json.dumps({k: manifest[k] for k in ['module_count','test_module_count','accepted_count','rejected_count','source_bytes','archive_bytes','oracle_compressed_bytes','oracle_uncompressed_bytes','archive_sha256','oracle_sha256']}, indent=2))

if __name__ == '__main__':
    main()
