#!/usr/bin/env python3
"""Compare a pinned adapter against every independently generated release AST oracle."""
import argparse, gzip, hashlib, json, os, pathlib, subprocess, tempfile, sys
P = pathlib.Path
MAX_CHILD_OUTPUT = 128 * 1024 * 1024

def sha(path):
    with path.open('rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()

PROTOCOL = {'name':'v2-surrogate-codepoints','ast':'Recursive ast._fields; omit _attributes positions; exec/type_comments=False','literals':'bytes/complex/float/Ellipsis literal_type+CPython repr; strings containing surrogate codepoints literal_type=str/codepoints; ordinary strings unchanged'}

def canonical_load(text):
    # Canonical JSON integers are not Python source-literal admission.
    previous = sys.get_int_max_str_digits()
    try:
        sys.set_int_max_str_digits(0)
        return json.loads(text)
    finally:
        sys.set_int_max_str_digits(previous)

def ast_equal(actual, expected):
    # Python equality alone conflates bool/int and int/float JSON values.
    if type(actual) is not type(expected):
        return False
    if isinstance(expected, dict):
        return actual.keys() == expected.keys() and all(ast_equal(actual[k], v) for k, v in expected.items())
    if isinstance(expected, list):
        return len(actual) == len(expected) and all(ast_equal(a, b) for a, b in zip(actual, expected))
    return actual == expected

def main():
    ap = argparse.ArgumentParser()
    assert not sys.flags.optimize
    ap.add_argument('--baseline-manifest', type=P, required=True)
    ap.add_argument('--baseline-oracle', type=P, required=True)
    ap.add_argument('--stdlib-root', type=P, help='Explicit relocated root; complete paths/hashes are verified')
    ap.add_argument('--adapter', type=P, required=True)
    ap.add_argument('--adapter-sha256', required=True)
    ap.add_argument('--output', type=P, required=True)
    ap.add_argument('--manifest', type=P, required=True)
    ap.add_argument('--oracle', type=P, required=True)
    args = ap.parse_args()
    baseline_path = args.baseline_manifest.resolve(strict=True)
    baseline_oracle = args.baseline_oracle.resolve(strict=True)
    baseline_sha = sha(baseline_path)
    baseline = canonical_load(baseline_path.read_text())
    manifest_path = args.manifest.resolve(strict=True)
    oracle_path = args.oracle.resolve(strict=True)
    manifest_sha = sha(manifest_path)
    manifest = canonical_load(manifest_path.read_text())
    assert manifest['schema'] == 1 and manifest['release_version'] == '3.12.3'
    assert manifest['literal_normalization_protocol'] == PROTOCOL['name']
    assert manifest['protocol_descriptor'] == PROTOCOL
    descriptor = json.dumps(PROTOCOL, sort_keys=True, separators=(',', ':')).encode()
    assert hashlib.sha256(descriptor).hexdigest() == manifest['protocol_descriptor_sha256']
    assert manifest['original_manifest_sha256'] == baseline_sha
    assert manifest['original_oracle_sha256'] == baseline['oracle_sha256']
    assert manifest['files'] == baseline['files'] and manifest['module_count'] == 1774
    assert (manifest['accepted_count'], manifest['rejected_count']) == (1765, 9)
    assert manifest['rejected_files'] == baseline['rejected_files']
    root = (args.stdlib_root or P(manifest['stdlib_root'])).resolve(strict=True)
    assert sorted(str(p.relative_to(root)) for p in root.rglob('*.py') if p.is_file()) == [x['path'] for x in manifest['files']]
    assert manifest['archive_sha256'] == '56bfef1fdfc1221ce6720e43a661e3eb41785dd914ce99698d8c7896af4bdaa1'
    assert manifest['complete_release_lib_py'] is True
    assert manifest['corpus_reused_without_extraction'] is True
    assert sha(baseline_oracle) == baseline['oracle_sha256']
    # Validate the ENTIRE input before invoking even one adapter process.
    records = 0
    rejected = []
    with gzip.open(oracle_path, 'rt', encoding='ascii') as stream:
        for index, line in enumerate(stream):
            assert index < 1774
            record = canonical_load(line)
            assert record['source'] == manifest['files'][index]
            expected = record['oracle']
            assert type(expected['accepted']) is bool
            if expected['accepted']:
                assert isinstance(expected['ast'], dict)
            else:
                rejected.append({'path': record['source']['path'], 'error_type': expected['error_type']})
            source_path = root / record['source']['path']
            assert sha(source_path) == record['source']['source_sha256']
            assert source_path.stat().st_size == record['source']['bytes']
            records += 1
    assert records == 1774 and rejected == manifest['rejected_files']
    adapter = args.adapter.resolve(strict=True)
    assert sha(adapter) == args.adapter_sha256
    assert sha(oracle_path) == manifest['oracle_sha256']
    args.output.mkdir(mode=0o700, exist_ok=False)
    counts = {'matched': 0, 'unsupported': 0, 'mismatch': 0, 'process_error': 0}
    entries = []
    report = {'accepted': False, 'literal_normalization_protocol': PROTOCOL['name'], 'manifest_path': str(manifest_path), 'oracle_path': str(oracle_path), 'whole_upstream_stdlib': True, 'manifest_sha256': sha(manifest_path), 'oracle_sha256': sha(oracle_path), 'adapter_sha256': args.adapter_sha256, 'counts': counts, 'entries': entries}
    def save():
        temporary = args.output / 'report.tmp'
        with temporary.open('w') as f:
            json.dump(report, f, indent=2)
            f.write('\n')
            f.flush()
            os.fsync(f.fileno())
        temporary.replace(args.output / 'report.json')
    save()
    with gzip.open(oracle_path, 'rt', encoding='ascii') as stream:
        for index, line in enumerate(stream):
            record = canonical_load(line)
            assert record['source'] == manifest['files'][index]
            path = root / record['source']['path']
            assert sha(path) == record['source']['source_sha256'] and sha(adapter) == args.adapter_sha256
            entry = {'path': record['source']['path'], 'oracle_accepted': record['oracle']['accepted']}
            stdout_path = args.output / f'{index:04d}.stdout'
            stderr_path = args.output / f'{index:04d}.stderr'
            entry.update(stdout_path=str(stdout_path), stderr_path=str(stderr_path))
            report['current_attempt'] = entry
            save()
            try:
                with stdout_path.open('xb') as stdout, stderr_path.open('xb') as stderr:
                    result = subprocess.run([str(adapter), str(path)], input=b'', stdout=stdout, stderr=stderr, timeout=10, check=False)
                entry['original_status'] = result.returncode
                entry['stdout_sha256'] = sha(stdout_path)
                entry['stderr_sha256'] = sha(stderr_path)
                save()  # Durable actual attempt before JSON comparison or post-binding checks.
                assert stdout_path.stat().st_size <= MAX_CHILD_OUTPUT and stderr_path.stat().st_size <= MAX_CHILD_OUTPUT
                if result.returncode == 2:
                    entry['classification'] = 'unsupported'
                elif result.returncode != 0:
                    entry['classification'] = 'process_error'
                else:
                    actual = canonical_load(stdout_path.read_bytes())
                    assert isinstance(actual, dict)
                    assert type(actual.get('accepted')) is bool
                    expected = record['oracle']
                    matched = actual['accepted'] == expected['accepted'] and (not expected['accepted'] or ast_equal(actual.get('ast'), expected['ast']))
                    entry['classification'] = 'matched' if matched else 'mismatch'
            except (subprocess.TimeoutExpired, OSError, ValueError, AssertionError) as error:
                entry['classification'] = 'process_error'
                entry['error_type'] = type(error).__name__
                entry.setdefault('original_status', None)
            entry['stdout_sha256'] = sha(stdout_path) if stdout_path.exists() else None
            entry['stderr_sha256'] = sha(stderr_path) if stderr_path.exists() else None
            counts[entry['classification']] += 1
            entries.append(entry)
            report.pop('current_attempt', None)
            save()
            assert sha(path) == record['source']['source_sha256'] and sha(adapter) == args.adapter_sha256
    assert len(entries) == 1774
    assert sha(manifest_path) == manifest_sha and sha(oracle_path) == manifest['oracle_sha256']
    assert sha(baseline_path) == baseline_sha and sha(baseline_oracle) == baseline['oracle_sha256']
    for item in manifest['files']:
        assert sha(root / item['path']) == item['source_sha256']
    report['release_lib_py_exact_parity'] = counts['matched'] == len(entries)
    report['accepted'] = report['release_lib_py_exact_parity']
    report['whole_upstream_stdlib'] = True
    save()
    return 0 if report['accepted'] else 1

if __name__ == '__main__':
    raise SystemExit(main())
