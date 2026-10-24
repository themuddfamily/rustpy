#!/usr/bin/env bash
# Prints the release notes of a version from CHANGELOG.md: the Markdown under its
# "## [X.Y.Z]" heading (also "## X.Y.Z" or "## vX.Y.Z", with or without " - date") up to the
# next "## " heading, without trailing link definitions. Falls back to the section of the
# release a pre-release leads to (0.3.0 for 0.3.0-eap.1), then to "## [Unreleased]". The IDE's
# What's New page uses the same lookup (crates/rustpy-updates/src/changelog.rs).
#
# Usage: scripts/release-notes.sh VERSION [CHANGELOG]
#   VERSION    e.g. 0.2.0, v0.2.0 or 0.3.0-eap.1
#   CHANGELOG  default: CHANGELOG.md in the repository root
# Exits with status 1 (printing a pointer to the changelog) when no section has notes.
set -euo pipefail

usage() { sed -n '2,/^set -euo pipefail$/{/^set /d;s/^# \{0,1\}//;p;}' "$0"; }
if [[ ${1:-} == -h || ${1:-} == --help ]]; then
  usage
  exit 0
fi
(($# >= 1 && $# <= 2)) || { usage >&2; exit 2; }
VERSION="${1#v}"
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
CHANGELOG="${2:-$ROOT/CHANGELOG.md}"
[[ -f $CHANGELOG ]] || { echo "release-notes: $CHANGELOG not found" >&2; exit 2; }

# section LABEL: prints the section whose heading label is LABEL (case-insensitive for
# "Unreleased"; versions compare without a leading "v" and without build metadata).
section() {
  awk -v want="$1" '
    function label(line,    s) {
      s = substr(line, 4)
      sub(/^[ \t]+/, "", s)
      sub(/^\[/, "", s)
      sub(/[] \t].*$/, "", s)
      sub(/^[vV]/, "", s)
      sub(/\+.*$/, "", s)
      return s
    }
    function norm(v) {
      sub(/^[vV]/, "", v)
      sub(/\+.*$/, "", v)
      return tolower(v)
    }
    BEGIN { want = norm(want); found = 0; fence = 0; n = 0 }
    {
      heading = !fence && ($0 ~ /^## / || $0 ~ /^# /)
      if ($0 ~ /^[ \t]*(```|~~~)/) fence = !fence
      if (heading) {
        if (found) exit
        if ($0 ~ /^## / && tolower(label($0)) == want) found = 1
        next
      }
      if (found) lines[++n] = $0
    }
    END {
      # Drop trailing blank lines and link definitions ("[0.2.0]: https://...").
      while (n > 0 && (lines[n] ~ /^[ \t]*$/ || lines[n] ~ /^[ \t]*\[[^]]+\]:[ \t]*[^ \t]+[ \t]*$/)) n--
      start = 1
      while (start <= n && lines[start] ~ /^[ \t]*$/) start++
      for (i = start; i <= n; i++) print lines[i]
      exit (start <= n ? 0 : 1)
    }
  ' "$CHANGELOG"
}

candidates=("$VERSION")
if [[ $VERSION == *-* ]]; then
  candidates+=("${VERSION%%-*}")
fi
candidates+=(Unreleased)
for c in "${candidates[@]}"; do
  if notes="$(section "$c")"; then
    printf '%s\n' "$notes"
    exit 0
  fi
done
echo "See CHANGELOG.md for the changes in this release."
exit 1
