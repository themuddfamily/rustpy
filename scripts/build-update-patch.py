#!/usr/bin/env python3
"""Explicit release-operator tool: produce one-binary bsdiff patch + checked manifest.
No publishing, credentials, tool installation or network access.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
parser = argparse.ArgumentParser()
parser.add_argument('--old', type=Path, required=True)
parser.add_argument('--new', type=Path, required=True)
parser.add_argument('--from-version', required=True)
parser.add_argument('--to-version', required=True)
parser.add_argument('--full-artifact-name', required=True)
parser.add_argument('--out', type=Path, required=True)
def main():
    args = parser.parse_args()
    if Path(args.full_artifact_name).name != args.full_artifact_name:
        parser.error('full artifact name must be a filename')
    args.out.mkdir(parents=True, exist_ok=True)
    name = f'{args.full_artifact_name}.from-{args.from_version}'
    patch = args.out / f'{name}.bsdiff'
    subprocess.run(['bsdiff', str(args.old), str(args.new), str(patch)], check=True)
    digest = lambda path: hashlib.file_digest(path.open('rb'), 'sha256').hexdigest()
    manifest = args.out / f'{name}.patch.json'
    manifest.write_text(json.dumps({'from': args.from_version, 'to': args.to_version,
        'artifact': patch.name, 'old_sha256': digest(args.old), 'new_sha256': digest(args.new)}, indent=2))
    with (args.out / 'SHA256SUMS').open('a') as sums:
        for path in (patch, manifest):
            sums.write(f'{digest(path)}  {path.name}\n')
if __name__ == '__main__':
    main()
