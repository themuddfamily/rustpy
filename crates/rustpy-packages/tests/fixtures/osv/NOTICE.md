# OSV advisory fixture attribution

These JSON files are captured OSV-format responses containing advisory material
from the following databases. The original capture did not record an upstream
Git commit or retrieval date; no such revision or date is asserted here.
The embedded modified/published timestamps describe advisories, not capture time.

- GitHub Advisory Database, GitHub and its contributors:
  https://github.com/github/advisory-database — CC BY 4.0;
  full terms in LICENSE-GITHUB.txt.
- Python Packaging Advisory Database, PyPA and its contributors:
  https://github.com/pypa/advisory-database — CC BY 4.0;
  full terms in LICENSE-PYPA.txt.

License sources: https://github.com/github/advisory-database/blob/main/LICENSE.md
and https://github.com/pypa/advisory-database/blob/main/LICENSE.
The license texts were retrieved on 2026-10-22. No endorsement is implied.

Individual advisory content is retained without alteration in the existing
OSV-format JSON records. Source URLs below are recorded in each payload.
The batch fixture contains identifiers and modification timestamps corresponding
to those databases, including records not fetched as individual fixtures.

| Fixture | Upstream source | SHA-256 of retained JSON |
| --- | --- | --- |
| GHSA-j8r2-6x86-q33q.json | https://github.com/github/advisory-database/blob/main/advisories/github-reviewed/2023/05/GHSA-j8r2-6x86-q33q/GHSA-j8r2-6x86-q33q.json | `c25e95e884d6c3fc36c430e216da2d78faba494be84f1b25c114a6627b470e7c` |
| GHSA-x84v-xcm2-53pg.json | https://github.com/github/advisory-database/blob/main/advisories/github-reviewed/2018/10/GHSA-x84v-xcm2-53pg/GHSA-x84v-xcm2-53pg.json | `972fea8bddb69d8a517127d329d3c60607b3479968fc1abf31c0225107f38c88` |
| PYSEC-2018-28.json | https://github.com/pypa/advisory-database/blob/main/vulns/requests/PYSEC-2018-28.yaml | `e0cb062f86d9424ac76eff541097fa5aca18264031dbd35c9a34114dd4b43403` |
| PYSEC-2023-74.json | https://github.com/pypa/advisory-database/blob/main/vulns/requests/PYSEC-2023-74.yaml | `a8cf62be7bef83ee3f45b601756932152c5f8f050c3ad74a07a8b275a97a5913` |
| PYSEC-2026-2275.json | https://github.com/pypa/advisory-database/blob/main/vulns/requests/PYSEC-2026-2275.yaml | `2d188f6a77311e237c8eba71532b8593adc620009529951a3ea2ee6dd7a72c18` |
| querybatch-requests-2.19.0.json | OSV querybatch API: https://api.osv.dev/v1/querybatch (requests 2.19.0 and an empty result) | `e6b11f75b08cbec798052d0e4f469c99515bed094ffb4bb7c630e1f7c9c02a43` |
