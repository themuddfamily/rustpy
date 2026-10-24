"""Regression coverage for license selection, retained evidence and native gates."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]


def load(name, relative):
    spec = importlib.util.spec_from_file_location(name, ROOT / relative)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


CARGO = load('cargo_license_generation', 'scripts/generate-cargo-notices.py')
NATIVE = load('native_license_collection', 'scripts/collect-native-licenses.py')


class LicenseEvidenceTests(unittest.TestCase):
    def test_combined_obligations_survive_permissive_alternative_selection(self):
        self.assertEqual(CARGO.selected_terms('(Apache-2.0 OR MIT) AND BSD-3-Clause'), ['BSD-3-Clause', 'MIT'])
        self.assertEqual(CARGO.selected_terms('MIT OR Apache-2.0 OR LGPL-2.1-or-later'), ['MIT'])
        self.assertEqual(CARGO.selected_terms('Apache-2.0 OR GPL-2.0-only'), ['Apache-2.0'])
        self.assertEqual(CARGO.selected_terms('(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0'), ['MIT', 'OFL-1.1', 'Ubuntu-font-1.0'])
        self.assertEqual(CARGO.selected_terms('MIT / Apache-2.0'), ['MIT'])
        with self.assertRaises(ValueError):
            CARGO.selected_terms('LicenseRef-Unreviewed')

    def test_stale_lock_is_rejected_before_package_processing(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'packaging').mkdir()
            (root / 'Cargo.lock').write_text('changed resolution')
            (root / CARGO.MANIFEST).write_text(json.dumps({'input_sha256': {'Cargo.lock': CARGO.digest(b'old resolution')}}))
            with patch.object(CARGO, 'ROOT', root):
                with self.assertRaisesRegex(ValueError, 'Cargo.lock changed'):
                    CARGO.check()

    def test_missing_covered_source_preserves_previously_reviewed_archive(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / 'covered.tar.gz'
            destination.write_bytes(b'previous reviewed archive')
            class Missing:
                def archive(self, package):
                    return None
            package = {'name': 'option-ext', 'version': '0.2.0'}
            with patch.object(CARGO, 'locked_packages', return_value={'option-ext@0.2.0': package}):
                with self.assertRaisesRegex(ValueError, 'Missing authenticated covered source'):
                    CARGO.source_bundle(Missing(), destination)
            self.assertEqual(destination.read_bytes(), b'previous reviewed archive')
            self.assertFalse(destination.with_name('covered.tar.gz.partial').exists())

    def fixture(self, root):
        binary = root / 'rustpy'
        binary.write_bytes(b'primary executable fixture')
        manifest = root / 'evidence.json'
        manifest.write_text(json.dumps({'binary_sha256': NATIVE.digest(binary), 'system_static_components': [], 'components': []}))
        output = root / 'collected'
        output.mkdir()
        return binary, manifest, output

    def test_extensionless_elf_runtime_cannot_bypass_notices(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary, manifest, output = self.fixture(root)
            (root / 'appimage-runtime').write_bytes(b'\x7fELF' + bytes(64))
            with self.assertRaisesRegex(ValueError, 'uncovered redistributed native'):
                NATIVE.collect_runtime(manifest, root, output, binary, 'packaged')

    def test_static_linkage_requires_explicit_declaration_and_real_binary_hash(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary, manifest, output = self.fixture(root)
            with self.assertRaisesRegex(ValueError, 'runtime-manifest is required'):
                NATIVE.collect_runtime(None, root, output, binary, 'packaged')
            data = json.loads(manifest.read_text())
            del data['system_static_components']
            manifest.write_text(json.dumps(data))
            with self.assertRaisesRegex(ValueError, 'system_static_components'):
                NATIVE.collect_runtime(manifest, root, output, binary, 'packaged')
            data['system_static_components'] = []
            data['binary_sha256'] = None
            manifest.write_text(json.dumps(data))
            with self.assertRaisesRegex(ValueError, 'binary_sha256'):
                NATIVE.collect_runtime(manifest, root, output, binary, 'packaged')

    def test_complete_native_evidence_is_retained_and_hash_bound(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary, manifest, output = self.fixture(root)
            runtime = root / 'runtime'
            runtime.mkdir()
            library = runtime / 'libfixture.so.1'
            library.write_bytes(b'fixture shared object')
            for name, content in [('COPYING', 'terms'), ('source.tar.gz', 'exact source fixture'), ('build.txt', 'build instructions'), ('replace.txt', 'replacement instructions')]:
                (root / name).write_text(content)
            data = json.loads(manifest.read_text())
            data['components'] = [{'name': 'fixture', 'version': '1', 'files': {'libfixture.so.1': NATIVE.digest(library)}, 'license_files': ['COPYING'], 'source_archives': {'source.tar.gz': NATIVE.digest(root / 'source.tar.gz')}, 'build_instructions': 'build.txt', 'replacement_instructions': 'replace.txt'}]
            manifest.write_text(json.dumps(data))
            NATIVE.collect_runtime(manifest, runtime, output, binary, 'pre-sign')
            retained = json.loads((output / 'runtime-manifest.json').read_text())
            self.assertEqual(retained['binary_stage'], 'pre-sign')
            records = retained['components'][0]['retained_evidence']
            self.assertEqual(len(records), 4)
            for record in records:
                self.assertEqual(NATIVE.digest(output / record['path']), record['sha256'])


if __name__ == '__main__':
    unittest.main()
