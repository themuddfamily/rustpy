#!/usr/bin/env python3
"""Generate a release checksum index with exact downloadable asset basenames."""
import argparse
import hashlib
from pathlib import Path

def checksums(directories):
    found = {}
    for directory in directories:
        for path in directory.rglob('*'):
            if not path.is_file() or not path.name.endswith(('.tar.gz', '.zip', '.exe', '.msi', '.dmg', '.AppImage', '.deb', '.rpm', '.flatpak', '.snap', '.bsdiff', '.patch.json')):
                continue
            if path.name in found:
                raise ValueError(f'Duplicate release asset name: {path.name}')
            with path.open('rb') as stream:
                found[path.name] = hashlib.file_digest(stream, 'sha256').hexdigest()
    if not found:
        raise ValueError('No release assets found')
    return ''.join(f'{digest}  {name}\n' for name, digest in sorted(found.items()))

if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('directories', type=Path, nargs='+')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    args.output.write_text(checksums(args.directories))
