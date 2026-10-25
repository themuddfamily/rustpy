"""Source contracts for release packaging and installer associations."""
from pathlib import Path
import unittest
import xml.etree.ElementTree as ET
ROOT = Path(__file__).resolve().parents[1]
class DistributionContracts(unittest.TestCase):
    def test_release_matrix_and_disabled_jobs(self):
        workflow = (ROOT / '.github/workflows/release.yml').read_text()
        for name in ('aarch64-unknown-linux-gnu', 'x86_64-unknown-linux-gnu', 'aarch64-pc-windows-msvc', 'x86_64-pc-windows-msvc', 'universal', 'SHA256SUMS'):
            self.assertIn(name, workflow)
        self.assertNotIn('softprops/action-gh-release', workflow)
        self.assertIn('if: ${{ false }}', workflow)
    def test_msi_associations_are_quoted(self):
        tree = ET.parse(ROOT / 'packaging/windows/rustpy.wxs')
        namespace = {'w': 'http://wixtoolset.org/schemas/v4/wxs'}
        registry = {node.attrib['Key']: node.attrib for node in
                    tree.findall('.//w:RegistryValue', namespace)
                    if 'Name' not in node.attrib}
        for suffix, prog_id in (('py', 'RustPy.Python'), ('pyi', 'RustPy.PythonStub'),
                                ('ipynb', 'RustPy.Notebook')):
            association = registry[rf'Software\Classes\.{suffix}']
            self.assertEqual(association['Root'], 'HKCU')
            self.assertEqual(association['Value'], prog_id)
            command = registry[rf'Software\Classes\{prog_id}\shell\open\command']
            self.assertEqual(command['Root'], 'HKCU')
            self.assertEqual(command['Value'], '"[INSTALLFOLDER]rustpy.exe" "%1"')
    def test_archive_and_launcher_contract(self):
        self.assertIn('tar -C "$stage"', (ROOT / 'packaging/linux/build-tar.sh').read_text())
        self.assertIn('Contents/bin/rustpy', (ROOT / 'packaging/macos/build-app.sh').read_text())
