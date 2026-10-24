"""Source contract fixtures. Deliberately not executed during feature implementation."""
from pathlib import Path
import unittest
ROOT = Path(__file__).resolve().parents[1]
class DistributionContracts(unittest.TestCase):
    def test_release_matrix_and_disabled_jobs(self):
        workflow = (ROOT / '.github/workflows/release.yml').read_text()
        for name in ('aarch64-unknown-linux-gnu', 'x86_64-unknown-linux-gnu', 'aarch64-pc-windows-msvc', 'x86_64-pc-windows-msvc', 'universal', 'SHA256SUMS'):
            self.assertIn(name, workflow)
        self.assertNotIn('softprops/action-gh-release', workflow)
        self.assertIn('if: ${{ false }}', workflow)
    def test_msi_associations_are_quoted(self):
        source = (ROOT / 'packaging/windows/rustpy.wxs').read_text()
        for suffix in ('py', 'pyi', 'ipynb'):
            self.assertIn(f'Extension Id="{suffix}"', source)
        self.assertIn('&quot;[#RustPyExe]&quot; &quot;%1&quot;', source)
    def test_archive_and_launcher_contract(self):
        self.assertIn('tar -C "$stage"', (ROOT / 'packaging/linux/build-tar.sh').read_text())
        self.assertIn('Contents/bin/rustpy', (ROOT / 'packaging/macos/build-app.sh').read_text())
