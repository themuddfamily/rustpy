"""Vendor object-code evidence remains narrow and bound to the actual executable."""
import copy
from io import BytesIO
import importlib.util
import json
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent


def load(name, relative):
    spec = importlib.util.spec_from_file_location(name, ROOT / relative)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


COLLECTOR = load('vendor_native_collector', 'scripts/collect-native-licenses.py')
PRODUCER = load('windows_native_producer', 'scripts/prepare-windows-native-evidence.py')


def ar_archive(members):
    data = bytearray(b'!<arch>\n')
    for name, payload in members:
        data.extend((name + '/').ljust(16).encode())
        data.extend(b'0'.ljust(12) + b'0'.ljust(6) + b'0'.ljust(6) + b'100644'.ljust(8))
        data.extend(str(len(payload)).ljust(10).encode() + b'`\n' + payload)
        if len(payload) % 2:
            data.extend(b'\n')
    return bytes(data)


class VendorEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='rustpy-vendor-evidence-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.binary = self.root / 'rustpy.exe'
        pe = bytearray(70)
        pe[:2] = b'MZ'
        pe[60:64] = (64).to_bytes(4, 'little')
        pe[64:68] = b'PE\0\0'
        pe[68:70] = (0x8664).to_bytes(2, 'little')
        self.binary.write_bytes(pe)
        self.sha = COLLECTOR.digest(self.binary)
        for name in ('terms.txt', 'redist.txt', 'map.txt', 'trace.txt', 'build.txt', 'replace.txt'):
            (self.root / name).write_text(name + ': retained fixture evidence\n')
        (self.root / 'record.json').write_text(json.dumps({'link': {
            'binary_sha256': self.sha, 'rustc_commit': 'b' * 40,
            'link_map_sha256': COLLECTOR.digest(self.root / 'map.txt'),
            'link_trace_sha256': COLLECTOR.digest(self.root / 'trace.txt')},
            'toolchain': {'target': 'x86_64-pc-windows-msvc'}}))
        (self.root / 'inputs.json').write_text(json.dumps({
            'binary_sha256': self.sha, 'runtime_family': 'msvc-crt',
            'inputs': [{'name': 'msvc-crt/lib/x64/libcmt.lib', 'sha256': 'c' * 64}]}))
        self.item = {
            'name': 'Microsoft Visual C++ CRT', 'version': '14.44.35211',
            'evidence_kind': 'microsoft-redistributable', 'vendor': 'Microsoft',
            'runtime_family': 'msvc-crt', 'review_basis': 'Object-code redistribution with retained vendor terms',
            'provenance_urls': ['https://learn.microsoft.com/en-us/visualstudio/releases/2022/redistribution'],
            'license_files': ['terms.txt'], 'vendor_terms': self.hashes('terms.txt'),
            'redistribution_evidence': self.hashes('redist.txt'),
            'linkage_evidence': self.hashes('map.txt', 'trace.txt', 'record.json', 'inputs.json'),
            'linkage_records': {'map': 'map.txt', 'trace': 'trace.txt', 'build_record': 'record.json'},
            'input_inventory': 'inputs.json', 'build_instructions': 'build.txt',
            'replacement_instructions': 'replace.txt',
        }

    def hashes(self, *names):
        return {name: COLLECTOR.digest(self.root / name) for name in names}

    def validate(self, item=None):
        return COLLECTOR.vendor_evidence_files(item or self.item, self.root, self.binary, self.sha)

    def test_windows_vendor_terms_and_link_inventory_are_retained_without_fake_sources(self):
        files = dict(self.validate())
        self.assertEqual(files['terms.txt'], COLLECTOR.digest(self.root / 'terms.txt'))
        self.assertIn('inputs.json', files)
        self.assertNotIn('source_archives', self.item)
        manifest = self.root / 'runtime.json'
        manifest.write_text(json.dumps({'binary_sha256': self.sha, 'components': [],
                                        'system_static_components': [self.item]}))
        output = self.root / 'collected'
        output.mkdir()
        COLLECTOR.collect_runtime(manifest, self.root / 'empty-runtime', output, self.binary, 'packaged')
        self.assertTrue((output / 'runtime-manifest.json').is_file())

    def test_vendor_category_cannot_be_applied_to_elf_or_an_arbitrary_component(self):
        original = self.binary.read_bytes()
        self.binary.write_bytes(b'\x7fELF' + b'fixture')
        with self.assertRaisesRegex(ValueError, 'Windows PE'):
            self.validate()
        self.binary.write_bytes(original)
        for key, value in (('vendor', 'Other'), ('runtime_family', 'openssl'), ('name', 'OpenSSL')):
            with self.subTest(key=key):
                item = copy.deepcopy(self.item)
                item[key] = value
                with self.assertRaises(ValueError):
                    self.validate(item)

    def test_vendor_terms_and_link_evidence_must_match_their_actual_hashes(self):
        for key in ('vendor_terms', 'redistribution_evidence', 'linkage_evidence'):
            with self.subTest(key=key):
                item = copy.deepcopy(self.item)
                name = next(iter(item[key]))
                item[key][name] = '0' * 64
                with self.assertRaisesRegex(ValueError, 'SHA256 mismatch'):
                    self.validate(item)

    def test_vendor_inventory_must_bind_final_binary_and_real_library_names(self):
        for change in ({'binary_sha256': '0' * 64}, {'inputs': []},
                       {'inputs': [{'name': 'msvc-crt/../../wrong.lib', 'sha256': 'a' * 64}]},
                       {'inputs': [{'name': 'msvc-crt/source.py', 'sha256': 'a' * 64}]}):
            with self.subTest(change=change):
                path = self.root / 'inputs.json'
                inventory = {'binary_sha256': self.sha, 'runtime_family': 'msvc-crt',
                             'inputs': [{'name': 'msvc-crt/libcmt.lib', 'sha256': 'a' * 64}]}
                inventory.update(change)
                path.write_text(json.dumps(inventory))
                item = copy.deepcopy(self.item)
                item['linkage_evidence']['inputs.json'] = COLLECTOR.digest(path)
                with self.assertRaises(ValueError):
                    self.validate(item)

    def test_vendor_build_record_and_source_bypass_are_rejected(self):
        for mutate in ('source', 'no_map', 'unknown_kind', 'no_terms'):
            with self.subTest(mutate=mutate):
                item = copy.deepcopy(self.item)
                if mutate == 'source':
                    item['source_archives'] = {'not-sources.lib': '0' * 64}
                elif mutate == 'no_map':
                    item['linkage_records'].pop('map')
                elif mutate == 'unknown_kind':
                    item['evidence_kind'] = 'skip-source'
                else:
                    item['vendor_terms'] = {}
                with self.assertRaises(ValueError):
                    self.validate(item)

    def test_another_executables_build_record_cannot_be_attached_to_this_manifest(self):
        path = self.root / 'record.json'
        record = json.loads(path.read_text())
        record['link']['binary_sha256'] = '0' * 64
        path.write_text(json.dumps(record))
        item = copy.deepcopy(self.item)
        item['linkage_evidence']['record.json'] = COLLECTOR.digest(path)
        with self.assertRaisesRegex(ValueError, 'bind the final executable'):
            self.validate(item)

    def test_ordinary_open_source_components_still_require_exact_source_archives(self):
        ordinary = {'name': 'OpenSSL', 'version': '3.0.0', 'license_files': ['terms.txt'],
                    'build_instructions': 'build.txt', 'replacement_instructions': 'replace.txt'}
        path = self.root / 'ordinary.json'
        path.write_text(json.dumps({'binary_sha256': self.sha, 'components': [],
                                    'system_static_components': [ordinary]}))
        output = self.root / 'ordinary-output'
        output.mkdir()
        with self.assertRaisesRegex(ValueError, 'exact corresponding source archives'):
            COLLECTOR.collect_runtime(path, self.root / 'empty-runtime', output, self.binary, 'packaged')


class WindowsProducerTests(unittest.TestCase):
    def test_vendor_terms_profile_matches_the_actual_visual_studio_major(self):
        legacy = PRODUCER.visual_studio_profile('17.14.39222.1')
        current = PRODUCER.visual_studio_profile('18.0.10517.64')
        self.assertEqual(legacy['year'], '2022')
        self.assertEqual(current['year'], '2026')
        self.assertNotEqual(legacy['tools_terms'], current['tools_terms'])
        self.assertIn('/2025/10/', current['runtime_terms'])
        self.assertTrue(current['redist'].endswith('/2026/redistribution'))
        for version in ('', '19.0.1', '18.0-preview', None):
            with self.subTest(version=version), self.assertRaises(ValueError):
                PRODUCER.visual_studio_profile(version)

    def test_expired_rustc_import_library_is_bound_to_its_retained_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            original = root / 'rustc-fixture' / 'raw-dylibs' / 'kernel32.lib'
            retained = root / 'native-inputs' / 'input.lib'
            retained.parent.mkdir()
            names = b'ImportedSymbol\0KERNEL32.dll\0'
            payload = struct.pack('<HHHHIIHH', 0, 0xffff, 0, 0x8664, 0, len(names), 0, 0) + names
            retained.write_bytes(ar_archive([('import.obj', payload)]))
            record = {'expanded_arguments': [str(original)], 'input_files': [{
                'path': str(original), 'sha256': COLLECTOR.digest(retained), 'retained': 'native-inputs/input.lib'}]}
            inputs = PRODUCER.linked_inputs(record, 'Loaded ' + str(original) + '(import.obj)', root)
            self.assertEqual(inputs, [retained.resolve()])
            PRODUCER.generated_import_archive(retained)
            retained.write_bytes(b'changed after linker capture')
            with self.assertRaisesRegex(ValueError, 'missing or changed'):
                PRODUCER.linked_inputs(record, '', root)

    def test_native_function_object_cannot_be_relabelled_as_an_import_stub(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'unreviewed.lib'
            header = struct.pack('<HHIIIHH', 0x8664, 1, 0, 0, 0, 0, 0)
            section = b'.text\0\0\0' + b'\0' * 28 + (0x20).to_bytes(4, 'little')
            path.write_bytes(ar_archive([('native.obj', header + section)]))
            with self.assertRaisesRegex(ValueError, 'code beyond import descriptors'):
                PRODUCER.generated_import_archive(path)

    def test_sdk_preflight_uses_installed_sdk_terms_and_refuses_missing_terms(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with self.assertRaisesRegex(ValueError, 'SDK terms are missing'):
                PRODUCER.installed_sdk_terms(root, '10.0.26100.0')
            terms = root / 'Licenses' / '10.0.26100.0' / 'SDKLicense.rtf'
            terms.parent.mkdir(parents=True)
            terms.write_text('Microsoft Windows SDK licence fixture')
            self.assertEqual(PRODUCER.installed_sdk_terms(root, '10.0.26100.0'), [terms])

    def test_vendor_document_download_does_not_accept_an_html_error_as_a_license(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'terms.docx'
            with patch.object(PRODUCER, 'urlopen', return_value=BytesIO(b'<html>Access denied</html>')):
                with self.assertRaisesRegex(ValueError, 'not a DOCX'):
                    PRODUCER.download('https://visualstudio.microsoft.com/terms.docx', path)
            self.assertFalse(path.exists())
            self.assertFalse(path.with_suffix('.docx.part').exists())

    def test_sdk_loader_objects_must_match_the_actual_linked_wrapper_archive(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'wrapper.rlib'
            loader = ar_archive([('loader.obj', b'matching loader'), ('crt.obj', b'helper')])
            path.write_bytes(ar_archive([('rust.o', b'rust code'), ('loader.obj', b'matching loader'), ('crt.obj', b'helper')]))
            PRODUCER.verify_bundled_loader(loader, [path])
            path.write_bytes(ar_archive([('loader.obj', b'changed loader')]))
            with self.assertRaisesRegex(ValueError, 'matching official SDK loader'):
                PRODUCER.verify_bundled_loader(loader, [path])

    def test_truncated_native_archive_is_not_accepted(self):
        with self.assertRaises(ValueError):
            list(PRODUCER.archive_members(b'!<arch>\n' + b'bad'))

    def test_explicit_input_and_loaded_archive_must_resolve_to_existing_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            native = root / 'libcmt.lib'
            native.write_bytes(b'archive fixture')
            record = {'expanded_arguments': [str(native)]}
            self.assertEqual(PRODUCER.linked_inputs(record, 'Loaded ' + str(native) + '(startup.obj)'), [native.resolve()])
            with self.assertRaisesRegex(ValueError, 'Cannot resolve'):
                PRODUCER.linked_inputs(record, 'Loaded missing.lib(startup.obj)')


if __name__ == '__main__':
    unittest.main()
