# Cargo upstream license text overrides

`../cargo-license-overrides.json` maps exact package versions to retained upstream
license and attribution files. Every entry records the retrieval URL and SHA-256
of the unmodified bytes. The notice generator verifies these hashes before use.
These files supplement published registry archives that omitted the relevant
license or attribution files; they do not replace any notices shipped in those
archives.

Ordinary entries use the exact Git revision in the published crate's
`.cargo_vcs_info.json`, retaining license and attribution files from the crate's
upstream directory and its ancestor directories. Shared repository/revision files
are stored once and referenced by every applicable package.

Exceptions are documented in each package's `reason`:

- `dispatch` explicitly declares MIT in its original upstream manifest but has no
  standalone license file. Its exact author/metadata evidence is retained beside
  the unmodified MIT template from a pinned SPDX license-list-data revision. No
  copyright statement or year has been invented, and the author field does not
  establish sole ownership.
- `selectors` explicitly declares MPL 2.0 in source headers. Its exact upstream
  `lib.rs` and the official Mozilla MPL 2.0 text are retained.
- The two `winapi` target import crates lack VCS metadata. All their published
  source, build, and import-library files match the corresponding target subtree
  at upstream tag `0.3.6`, commit
  `4ac6c90e4ab92850c78b6103137290fe4ab9d273`; both target manifests identify version
  `0.4.0`. The original manifest differs only in packaging include patterns: the
  tagged revision adds the omitted license files. Target-specific licenses from
  that exact revision are retained.
- `r-efi` stores its complete MIT permission text and author notices in `AUTHORS`.

Do not regenerate missing copyright statements from package author metadata.
When updating a dependency, inspect its own published VCS metadata and upstream
license/notice files again; do not silently reuse another release's override.
