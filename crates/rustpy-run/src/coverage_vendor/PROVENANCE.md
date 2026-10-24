# Bundled coverage runtime

coverage.py 7.6.1 is Apache-2.0; LICENSE.txt and NOTICE.txt are retained both
beside this file and inside the runtime archive. Package source files are
unmodified. RustPy repackaged the upstream source distribution on 2026-10-22
to keep only regular files under `coverage-7.6.1/coverage/` and those two
license/notice files. Tests, docs, build metadata and the unused jQuery
Templates test dependency are excluded from redistribution.

Upstream: https://pypi.org/project/coverage/7.6.1/#files
Original archive SHA-256: `953510dfb7b12ab69d20135a0662397f077c59b1e6379a768e97c59d852ee51d`
Runtime archive SHA-256: `49df48785d32233d6ab8b242ccc9f236e0d39f4c1c40e71c1e1c73d7b3476c4c`

`runtime-manifest.json` records every retained member's original content hash
and mode and the deterministic archive normalization. The archive keeps its
versioned filename for the existing bootstrap; it is a runtime subset, not
the original upstream source distribution.

tomli 2.0.1 remains the unmodified source distribution under the MIT license
retained in TOMLI-LICENSE and inside its archive.

Deferred verification: run `python3 verify_resources.py` from this directory.
