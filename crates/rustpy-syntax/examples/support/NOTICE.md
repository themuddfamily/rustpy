# Frozen CPython and Unicode data

The Unicode name table (`unicode15-names.tsv`), Unicode identifier ranges
(`unicode15_ident.rs`), and codec oracle table (`source-codec-tables.json`)
are derived data, not original RustPy character databases. Retain this notice
and the adjacent LICENSE-CPYTHON.txt and LICENSE-UNICODE.txt when redistributing
these files or their derived tables.

## Provenance

- `unicode15-names.tsv`: Unicode 15.0.0 canonical scalar names and formal
  aliases. Introduced in RustPy commit
  `414da6f92ccd002d2a8120b2192e79ed88233abb`. The accompanying source describes
  it as a CPython 3.12 / Unicode 15 table; the original generation command was
  not committed. The underlying sources are the Unicode Character Database
  15.0.0 (https://www.unicode.org/Public/15.0.0/ucd/), including UnicodeData.txt
  and NameAliases.txt, and CPython's algorithmic character names.
- `unicode15_ident.rs`: its original header records generation from CPython
  3.12 `str.isidentifier`. Introduced in RustPy commit
  `ff3866b75d795b26334e29dbf6427888e57fb9ca`. It stores compressed start and
  continuation ranges instead of Python's internal Unicode tables.
- `source-codec-tables.json`: the recorded oracle is CPython 3.12.3; introduced
  with the name table in commit `414da6f92ccd002d2a8120b2192e79ed88233abb`.
  It records the decoded scalar (or null for a decoding error) for every byte
  in `koi8-r` and `cp1252`. Upstream codec implementations are
  https://github.com/python/cpython/blob/v3.12.3/Lib/encodings/koi8_r.py and
  https://github.com/python/cpython/blob/v3.12.3/Lib/encodings/cp1252.py.
  The oracle's build timestamp describes that local interpreter build, not
  an upstream release or source revision.

`regenerate_tables.py` is a new reconstruction recipe for the names and codec
JSON, added after the historical capture. It requires CPython 3.12.3 and
Unicode 15.0.0, checks the versioned alias download's SHA-256, and supports
`--check` to compare the result without writing. It does not purport to be
that capture's missing generator. Multi-scalar named sequences are omitted
because this fixture serves scalar-name lookup.

## Licenses and changes

CPython 3.12.3 is available under the PSF license and historical license chain
retained verbatim in LICENSE-CPYTHON.txt, retrieved from
https://github.com/python/cpython/blob/v3.12.3/LICENSE.
Copyright (c) 2001-2023 Python Software Foundation; All Rights Reserved.
RustPy's changes extract name/scalar pairs, compress identifier membership into
Rust ranges, and serialize individual codec results as JSON. No CPython runtime
is included in these tables.

Unicode data is Copyright © 1991-2022 Unicode, Inc. All rights reserved.
The Unicode Data Files and Software permission notice (Unicode-DFS-2016) is
retained in LICENSE-UNICODE.txt. The historical notice was obtained from the
Unicode 15-era ICU 72.1 release's first license section:
https://github.com/unicode-org/icu/blob/release-72-1/icu4c/LICENSE.
Unicode 15.0.0's own ReadMe identifies Unicode, Inc. as the data copyright holder:
https://www.unicode.org/Public/15.0.0/ucd/ReadMe.txt.

These notices also cover equivalent derived codec constants in the neighboring
`source_decode.rs`. License texts were retrieved on 2026-10-22. These are
component-specific terms and do not replace RustPy's project license.
