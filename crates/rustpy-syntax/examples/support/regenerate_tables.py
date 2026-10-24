#!/usr/bin/env python3
"""Reconstruct the frozen name/codec tables; requires unmodified CPython 3.12.3.

Run with --check to compare without writing. Network access fetches only the
versioned Unicode 15.0.0 alias file, whose digest is checked before use.
This script was added after the original table capture, not recovered from it.
"""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import sys
import unicodedata
from urllib.request import urlopen

ALIAS_URL = 'https://www.unicode.org/Public/15.0.0/ucd/NameAliases.txt'
ALIAS_SHA256 = '3e39509e8fae3e5d50ba73759d0b97194501d14a9c63107a6372a46b38be18e8'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    if platform.python_implementation() != 'CPython' or sys.version_info[:3] != (3, 12, 3):
        parser.error('use CPython 3.12.3 from https://github.com/python/cpython/tree/v3.12.3')
    if unicodedata.unidata_version != '15.0.0':
        parser.error('requires Unicode 15.0.0')
    with urlopen(ALIAS_URL) as response:
        aliases = response.read()
    if hashlib.sha256(aliases).hexdigest() != ALIAS_SHA256:
        raise SystemExit('Unicode alias source digest mismatch')
    names = {}
    for codepoint in range(0x110000):
        name = unicodedata.name(chr(codepoint), None)
        if name is not None:
            names[name] = codepoint
    for line in aliases.decode('utf-8').splitlines():
        if line and not line.startswith('#'):
            codepoint, name, _kind = line.split(';')
            names[name] = int(codepoint, 16)
    root = Path(__file__).resolve().parent
    outputs = {'unicode15-names.tsv': ''.join(f'{name}\t{value:X}\n' for name, value in sorted(names.items()))}
    codecs = json.loads((root / 'source-codec-tables.json').read_text())
    # Preserve the historical oracle build string; it is not this process's build.
    for encoding in codecs['mapping']:
        mapping = []
        for value in range(256):
            try:
                mapping.append(ord(bytes([value]).decode(encoding)))
            except UnicodeDecodeError:
                mapping.append(None)
        codecs['mapping'][encoding] = mapping
    outputs['source-codec-tables.json'] = json.dumps(codecs, indent=2) + '\n'
    for filename, content in outputs.items():
        path = root / filename
        if args.check:
            if path.read_bytes() != content.encode('utf-8'):
                raise SystemExit(f'{filename}: reconstructed data differs')
        else:
            path.write_bytes(content.encode('utf-8'))


if __name__ == '__main__':
    main()
