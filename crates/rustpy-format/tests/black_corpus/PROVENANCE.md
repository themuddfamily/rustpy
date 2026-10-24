# Pinned Black conformance oracle

These are **all 197** `tests/data/cases/*.py` files from
[psf/black 26.1.0](https://github.com/psf/black/tree/6305bf1ae645ab7541be4f5028a86239316178eb/tests/data/cases),
commit `6305bf1ae645ab7541be4f5028a86239316178eb`, copied byte-for-byte.
The upstream MIT license is retained in LICENSE. manifest.json records each
original Git blob, SHA-256, and byte length. No cases were selected by results.

The offline harness follows upstream `tests/util.py`'s `# flags:` / `# output`
fixture protocol. It uses the checked-in expected output, never a locally
installed Black version, Python process, network service, or oracle cache.

Run `cargo run -p rustpy-format --example black_corpus` when verification is
permitted. The serial JSON report lists every case and counts exact, different,
refused, unsupported, unstable and comment-changing outcomes separately. Use
`-- --require-exact` to require every case to match. Preview/unstable, explicit
Python target, stub, and line-range fixture modes lacking equivalent formatter
options are explicitly unsupported, retained in the denominator. A refusal is
not an exact match. Long unbreakable string/comment payloads may exceed the
configured line width.

No conformance score has been measured for this revision during implementation.
The ordinary corpus test checks inventory and accepted-output safety/stability;
it does not turn best-effort exactness into an unsupported all-cases-pass claim.
