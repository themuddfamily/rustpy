"""Keep shared Cargo declarations in use by workspace members."""
from pathlib import Path
import tomllib
import unittest


ROOT = Path(__file__).resolve().parent.parent


class WorkspaceManifest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workspace = tomllib.loads((ROOT / 'Cargo.toml').read_text())['workspace']
        cls.members = [
            tomllib.loads((path / 'Cargo.toml').read_text())
            for pattern in cls.workspace['members']
            for path in ROOT.glob(pattern)
            if (path / 'Cargo.toml').is_file()
        ]

    def test_workspace_dependencies_have_consumers(self):
        used = set()
        for member in self.members:
            tables = [member, *member.get('target', {}).values()]
            for table in tables:
                for kind in ('dependencies', 'dev-dependencies', 'build-dependencies'):
                    used.update(name for name, value in table.get(kind, {}).items()
                                if isinstance(value, dict) and value.get('workspace'))
        self.assertEqual(set(self.workspace['dependencies']) - used, set())

    def test_workspace_package_fields_have_consumers(self):
        used = {name for member in self.members
                for name, value in member['package'].items()
                if isinstance(value, dict) and value.get('workspace')}
        self.assertEqual(set(self.workspace['package']) - used, set())


if __name__ == '__main__':
    unittest.main()
