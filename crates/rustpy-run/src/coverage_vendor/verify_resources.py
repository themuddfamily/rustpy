#!/usr/bin/env python3
"""Offline regression check of the redistributed coverage runtime and licensing."""
import hashlib
import io
import json
from pathlib import Path
import tarfile


def verify():
    root = Path(__file__).resolve().parent
    manifest = json.loads((root / 'runtime-manifest.json').read_text())
    archive = (root / 'coverage-7.6.1.tar.gz').read_bytes()
    assert hashlib.sha256(archive).hexdigest() == manifest['runtime_sha256']
    expected = {entry['path']: entry for entry in manifest['members']}
    with tarfile.open(fileobj=io.BytesIO(archive), mode='r:gz') as bundle:
        members = bundle.getmembers()
        assert len(members) == len(expected), 'duplicate or unexpected archive entries'
        assert {member.name for member in members} == set(expected)
        for member in members:
            assert member.isfile(), member.name
            assert member.uid == member.gid == member.mtime == 0
            assert member.mode == expected[member.name]['mode']
            content = bundle.extractfile(member).read()
            assert hashlib.sha256(content).hexdigest() == expected[member.name]['sha256']
            assert member.name.startswith('coverage-7.6.1/coverage/') or member.name in {
                'coverage-7.6.1/LICENSE.txt', 'coverage-7.6.1/NOTICE.txt'
            }, member.name
        for filename in ('LICENSE.txt', 'NOTICE.txt'):
            assert bundle.extractfile('coverage-7.6.1/' + filename).read() == (root / filename).read_bytes()
        for module in ('__init__.py', '__main__.py', 'cmdline.py', 'control.py', 'pytracer.py', 'html.py'):
            assert 'coverage-7.6.1/coverage/' + module in expected, module
        assert any(path.startswith('coverage-7.6.1/coverage/htmlfiles/') for path in expected)
    print('Coverage runtime inventory, content hashes and retained notices verified.')


if __name__ == '__main__':
    verify()
