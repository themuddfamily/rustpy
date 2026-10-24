# CPython 3.12 semantic oracle

`cpython_oracle` maps the Rust CST to the complete normalized CPython AST schema
without invoking Python. Every node's `_fields` participates in comparison;
location `_attributes` are intentionally excluded. The maintained fixtures target
**CPython 3.12.3**, not arbitrary future Python releases.

## Fixed semantic goldens

The repository contains 102 fixed primary-AST goldens, including control flow,
patterns, comprehensions, source codecs, Unicode identifiers and literal forms:

```sh
cargo test --locked -p rustpy-syntax --example cpython_oracle
cargo build --locked -p rustpy-syntax --example cpython_oracle
```

For one file, `target/debug/examples/cpython_oracle source.py` emits JSON.
A syntax rejection returns status 0 with `accepted: false`; unsupported semantic
mapping returns **status 2** and a diagnostic. Unsupported is never a match or a
fabricated AST. Other nonzero statuses and timeouts are process failures.

## Complete official release Lib corpus

The scripts in `support/stdlib/` reproduce all **1,774** regular `.py` files under
CPython 3.12.3's official `Lib/`, including **1,019** test modules. No Python file
is excluded: **1,765** are primary-parser accepted and **9** intentionally invalid
fixtures are primary-parser rejected. Non-Python assets are not extracted.

Use an independently installed **CPython 3.12.3** interpreter without `-O`.
The generator downloads only the fixed official URL (or takes `--archive`) and
requires archive SHA-256
`56bfef1fdfc1221ce6720e43a661e3eb41785dd914ce99698d8c7896af4bdaa1`.
It extracts only bounded regular Lib `.py` members into a fresh output directory.
It never imports or executes corpus modules. Each oracle comes from actual
`ast.parse(source_bytes, mode="exec", type_comments=False)`.

```sh
python3.12 crates/rustpy-syntax/examples/support/stdlib/prepare_release.py --output /tmp/cpython-3.12.3-lib
python3.12 crates/rustpy-syntax/examples/support/stdlib/prepare_oracle_v2.py --baseline /tmp/cpython-3.12.3-lib --output /tmp/cpython-3.12.3-v2
cargo build --locked -p rustpy-syntax --example cpython_oracle
sha256sum target/debug/examples/cpython_oracle
python3.12 crates/rustpy-syntax/examples/support/stdlib/compare_stdlib.py --baseline-manifest /tmp/cpython-3.12.3-lib/manifest.json --baseline-oracle /tmp/cpython-3.12.3-lib/oracles.jsonl.gz --manifest /tmp/cpython-3.12.3-v2/manifest.json --oracle /tmp/cpython-3.12.3-v2/oracles.jsonl.gz --adapter target/debug/examples/cpython_oracle --adapter-sha256 REPLACE_WITH_ACTUAL_SHA256 --output /tmp/cpython-3.12.3-comparison
```

Every output directory must be new. `--stdlib-root` permits an explicit relocated
Lib root while verifying the entire exact inventory and each file's byte hash.
The same interpreter binary used to prepare the baseline must generate v2.
No corpus/binary is downloaded or generated merely by building/testing Rust.

Protocol `v2-surrogate-codepoints` preserves isolated surrogates and adjacent
surrogate pairs as individual scalar/codepoint records **before JSON serialization**;
it never reconstructs primary ASTs from old JSON. Bytes, complex values, floating
values and Ellipsis carry type plus CPython repr. Ordinary strings remain strings.
Recursive exact-type comparison prevents bool/int or int/float equality aliases.

The comparator validates all inputs before launching any adapter. It saves each
file's raw stdout/stderr, original status and classification and checkpoints a
fsynced report after every file. Exit 2, mismatches and process errors remain
distinct. Success requires all 1,774 records, including all 9 primary rejections,
to match. Scripts are reusable validation tools, not a claim that an unrun build
passes the corpus. Keep failed reports for diagnosis.
