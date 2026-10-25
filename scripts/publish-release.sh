#!/usr/bin/env bash
# Publish checked Windows assets, resuming an unpublished draft when present.
set -euo pipefail

if (( $# != 4 )); then
  echo "Usage: publish-release.sh TAG REVISION ASSET_DIRECTORY NOTES_FILE" >&2
  exit 2
fi
tag=$1
revision=$2
assets=$(realpath "$3")
notes=$(realpath "$4")
[[ -s "$notes" ]]
[[ $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]
[[ $revision =~ ^[0-9a-f]{40}$ ]]
version=${tag#v}
prerelease=false
latest=true
if [[ $version == *-* ]]; then prerelease=true; latest=false; fi
expected=()
for arch in x86_64 arm64; do
  expected+=("rustpy-$version-windows-$arch-portable.zip")
  expected+=("rustpy-$version-windows-$arch-setup.exe")
done
expected+=("rustpy-$version-windows-x64.msi" "rustpy-$version-windows-arm64.msi")

# A checksum index must describe exactly both complete architecture sets.
python3 - "$assets" "${expected[@]}" <<'PYTHON'
import hashlib
from pathlib import Path
import re
import sys

root = Path(sys.argv[1])
expected = set(sys.argv[2:])
entries = {}
for line in (root / 'SHA256SUMS').read_text().splitlines():
    match = re.fullmatch(r'([0-9a-f]{64})  (.+)', line)
    if not match or match[2] not in expected or match[2] in entries:
        raise SystemExit('Invalid, duplicate or unexpected release checksum entry')
    entries[match[2]] = match[1]
if set(entries) != expected:
    raise SystemExit('Release checksums must cover all six Windows packages')
for name, digest in entries.items():
    path = root / name
    if not path.is_file() or path.stat().st_size == 0:
        raise SystemExit(f'Missing or empty release asset: {name}')
    with path.open('rb') as source:
        if hashlib.file_digest(source, 'sha256').hexdigest() != digest:
            raise SystemExit(f'Release checksum mismatch: {name}')
PYTHON

if git ls-remote --exit-code --tags origin "refs/tags/$tag"; then
  echo "Refusing to replace existing tag $tag" >&2
  exit 1
else
  status=$?
  if (( status != 2 )); then exit "$status"; fi
fi

state=$(mktemp -d "${TMPDIR:-/tmp}/rustpy-publish.XXXXXXXX")
trap 'rm -rf "$state"' EXIT
# Listing distinguishes an absent release from authentication/network failures.
# Query every page: an older draft need not be on the first page.
gh api 'repos/{owner}/{repo}/releases' --paginate --slurp > "$state/releases.json"
release_state=$(python3 - "$state/releases.json" "$tag" "${expected[@]}" <<'PYTHON'
import json
from pathlib import Path
import sys

pages = json.loads(Path(sys.argv[1]).read_text())
matches = [release for page in pages for release in page if release['tag_name'] == sys.argv[2]]
if len(matches) > 1:
    raise SystemExit('Multiple releases use the requested tag')
if not matches:
    print('absent')
else:
    release = matches[0]
    if release['draft'] is not True:
        raise SystemExit('Refusing to replace a published release')
    expected = set(sys.argv[3:]) | {'SHA256SUMS'}
    if any(asset['name'] not in expected for asset in release['assets']):
        raise SystemExit('Existing draft has unexpected assets; review them before publication')
    print('draft')
PYTHON
)

if [[ $release_state == absent ]]; then
  gh release create "$tag" --target "$revision" --title "RustPy $tag" \
    --draft --prerelease="$prerelease" --notes-file "$notes"
else
  gh release edit "$tag" --target "$revision" --title "RustPy $tag" \
    --draft=true --prerelease="$prerelease" --notes-file "$notes"
fi

uploads=()
for name in "${expected[@]}" SHA256SUMS; do uploads+=("$assets/$name"); done
# Failed uploads leave a draft. A retry replaces only the expected draft assets.
gh release upload "$tag" "${uploads[@]}" --clobber
gh release edit "$tag" --draft=false --prerelease="$prerelease" --latest="$latest"
