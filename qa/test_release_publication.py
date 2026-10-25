"""Exercise publication ordering without contacting GitHub or changing Git tags."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent

FAKE_TOOL = '''#!{python}
import json
import os
from pathlib import Path
import sys

command = [Path(sys.argv[0]).name, *sys.argv[1:]]
with open(os.environ['PUBLICATION_LOG'], 'a') as log:
    log.write(json.dumps(command) + '\\n')
scenario = json.loads(os.environ['PUBLICATION_SCENARIO'])
if command[0] == 'git':
    sys.exit(scenario.get('git_status', 2))
if command[1] == 'api':
    if scenario.get('query_failure'):
        sys.exit(1)
    print(json.dumps(scenario.get('pages', [[]])))
elif command[1:3] == ['release', 'upload'] and scenario.get('upload_failure'):
    sys.exit(1)
'''


@unittest.skipUnless(shutil.which('bash') and shutil.which('realpath'), 'requires Bash and realpath')
class ReleasePublicationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='rustpy-publication-')
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.assets = self.directory / 'assets with spaces'
        self.assets.mkdir()
        self.tools = self.directory / 'tools'
        self.tools.mkdir()
        for name in ('gh', 'git'):
            path = self.tools / name
            path.write_text(FAKE_TOOL.format(python=sys.executable))
            path.chmod(0o755)
        self.notes = self.directory / 'release notes.md'
        self.notes.write_text('Candidate changelog and verified limitations.\n')
        self.log = self.directory / 'commands.jsonl'
        self.tag = 'v1.0.0'
        self.revision = 'a' * 40
        self.prepare_assets()

    def prepare_assets(self):
        version = self.tag[1:]
        self.names = [
            f'rustpy-{version}-windows-{arch}-{kind}'
            for arch in ('x86_64', 'arm64')
            for kind in ('portable.zip', 'setup.exe')
        ] + [f'rustpy-{version}-windows-{arch}.msi' for arch in ('x64', 'arm64')]
        entries = []
        for name in self.names:
            contents = name.encode()
            (self.assets / name).write_bytes(contents)
            entries.append(f'{hashlib.sha256(contents).hexdigest()}  {name}\n')
        (self.assets / 'SHA256SUMS').write_text(''.join(entries))

    def invoke(self, **scenario):
        environment = dict(os.environ)
        environment.update(
            PATH=str(self.tools) + os.pathsep + os.environ['PATH'],
            TMPDIR=str(self.directory),
            PUBLICATION_LOG=str(self.log),
            PUBLICATION_SCENARIO=json.dumps(scenario),
        )
        result = subprocess.run(
            ['bash', str(ROOT / 'scripts/publish-release.sh'), self.tag,
             self.revision, str(self.assets), str(self.notes)],
            env=environment, capture_output=True, text=True,
        )
        calls = [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []
        return result, calls

    def assert_not_published(self, calls):
        self.assertFalse(any('--draft=false' in call for call in calls), calls)

    def assert_successful_upload_then_publication(self, result, calls):
        self.assertEqual(result.returncode, 0, result.stderr)
        upload = next(call for call in calls if call[:3] == ['gh', 'release', 'upload'])
        self.assertEqual(set(upload[4:-1]), {str(self.assets / name) for name in self.names + ['SHA256SUMS']})
        self.assertEqual(upload[-1], '--clobber')
        self.assertEqual(calls[-2], upload)
        self.assertEqual(calls[-1][:4], ['gh', 'release', 'edit', self.tag])
        self.assertIn('--draft=false', calls[-1])

    def test_absent_release_is_created_as_draft_before_upload_and_publication(self):
        result, calls = self.invoke()
        self.assert_successful_upload_then_publication(result, calls)
        create = calls[-3]
        self.assertEqual(create[:4], ['gh', 'release', 'create', self.tag])
        self.assertIn('--draft', create)
        self.assertEqual(create[create.index('--target') + 1], self.revision)
        self.assertEqual(create[create.index('--notes-file') + 1], str(self.notes))
        self.assertIn('--latest=true', calls[-1])
        self.assertIn('--prerelease=false', calls[-1])
        self.assertEqual(calls[1][-2:], ['--paginate', '--slurp'])

    def test_existing_draft_on_later_page_gets_candidate_target_and_notes(self):
        result, calls = self.invoke(pages=[[], [{
            'tag_name': self.tag, 'draft': True,
            'assets': [{'name': self.names[0]}, {'name': 'SHA256SUMS'}],
        }]])
        self.assert_successful_upload_then_publication(result, calls)
        edit = calls[-3]
        self.assertEqual(edit[:4], ['gh', 'release', 'edit', self.tag])
        self.assertIn('--draft=true', edit)
        self.assertEqual(edit[edit.index('--target') + 1], self.revision)
        self.assertEqual(edit[edit.index('--notes-file') + 1], str(self.notes))
        self.assertFalse(any(call[:3] == ['gh', 'release', 'create'] for call in calls))

    def test_published_release_is_not_replaced(self):
        result, calls = self.invoke(pages=[[{'tag_name': self.tag, 'draft': False, 'assets': []}]])
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('published release', result.stderr)
        self.assertEqual(len(calls), 2)
        self.assert_not_published(calls)

    def test_existing_tag_or_git_query_failure_prevents_any_github_call(self):
        for status in (0, 1, 128):
            with self.subTest(status=status):
                self.log.unlink(missing_ok=True)
                result, calls = self.invoke(git_status=status)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(len(calls), 1)
                self.assertEqual(calls[0][0], 'git')
                self.assert_not_published(calls)

    def test_release_query_failure_is_not_treated_as_absent(self):
        result, calls = self.invoke(query_failure=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(calls), 2)
        self.assert_not_published(calls)

    def test_upload_failure_leaves_new_or_existing_release_in_draft(self):
        for pages in ([[]], [[{'tag_name': self.tag, 'draft': True, 'assets': []}]]):
            with self.subTest(pages=pages):
                self.log.unlink(missing_ok=True)
                result, calls = self.invoke(pages=pages, upload_failure=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(calls[-1][:3], ['gh', 'release', 'upload'])
                self.assert_not_published(calls)

    def test_unexpected_existing_draft_assets_require_review(self):
        result, calls = self.invoke(pages=[[{
            'tag_name': self.tag, 'draft': True, 'assets': [{'name': 'old-installer.exe'}],
        }]])
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('unexpected assets', result.stderr)
        self.assertEqual(len(calls), 2)
        self.assert_not_published(calls)

    def test_missing_or_corrupt_package_prevents_remote_calls(self):
        asset = self.assets / self.names[0]
        asset.write_bytes(b'corrupt')
        result, calls = self.invoke()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, [])
        asset.unlink()
        result, calls = self.invoke()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, [])

    def test_incomplete_checksum_index_prevents_remote_calls(self):
        path = self.assets / 'SHA256SUMS'
        path.write_text('\n'.join(path.read_text().splitlines()[:-1]) + '\n')
        result, calls = self.invoke()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, [])

    def test_eap_release_is_not_promoted_to_latest_stable(self):
        self.tag = 'v1.0.0-eap.1'
        self.prepare_assets()
        result, calls = self.invoke()
        self.assert_successful_upload_then_publication(result, calls)
        self.assertIn('--prerelease=true', calls[-3])
        self.assertIn('--prerelease=true', calls[-1])
        self.assertIn('--latest=false', calls[-1])


if __name__ == '__main__':
    unittest.main()
