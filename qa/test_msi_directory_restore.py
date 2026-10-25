"""Guard custom-directory maintenance sequencing before native MSI acceptance."""
from pathlib import Path
import re
import unittest
import xml.etree.ElementTree as ET


ROOT = Path(__file__).resolve().parent.parent
NS = {'w': 'http://wixtoolset.org/schemas/v4/wxs'}


class MsiDirectoryRestoreTests(unittest.TestCase):
    def test_maintenance_acceptance_repairs_registry_keypath_components(self):
        package = ET.parse(ROOT / 'packaging/windows/rustpy.wxs').getroot()
        component = package.find(".//w:Component[@Id='Executable']", NS)
        self.assertIsNotNone(component.find("w:File[@Id='RustPyExe']", NS))
        key_path = component.find("w:RegistryValue[@KeyPath='yes']", NS)
        self.assertEqual(key_path.get('Root'), 'HKCU')

        source = (ROOT / 'scripts/test-windows-packages.ps1').read_text()
        repair = re.search(r"Invoke-Checked 'msi-core-repair' 'msiexec.exe' @\('(/f[a-z]+)'", source)
        self.assertIsNotNone(repair)
        self.assertTrue(set('amus') <= set(repair[1][2:]),
                        'Full repair must restore files, registry-keypath components and shortcuts')

    def test_saved_per_user_directory_is_restored_before_component_costing(self):
        package = ET.parse(ROOT / 'packaging/windows/rustpy.wxs').getroot()
        saved = package.find(".//w:Property[@Id='RUSTPY_INSTALLFOLDER']", NS)
        self.assertIsNotNone(saved)
        search = saved.find('w:RegistrySearch', NS)
        self.assertEqual(search.get('Root'), 'HKCU')
        self.assertEqual(search.get('Key'), r'Software\RustPy')
        self.assertEqual(search.get('Name'), 'InstallFolder')

        restore = package.find(".//w:SetProperty[@Id='INSTALLFOLDER']", NS)
        self.assertIsNotNone(restore, 'Restore the property before MSI resolves component actions')
        self.assertEqual(restore.get('Value'), '[RUSTPY_INSTALLFOLDER]')
        self.assertEqual(restore.get('Before'), 'CostFinalize')
        self.assertEqual(restore.get('Sequence'), 'both')
        self.assertEqual(restore.get('Condition'), 'Installed AND RUSTPY_INSTALLFOLDER')
        self.assertIsNone(package.find(".//w:SetDirectory[@Id='INSTALLFOLDER']", NS),
                          'A late directory setter invalidates already-costed maintenance paths')


if __name__ == '__main__':
    unittest.main()
