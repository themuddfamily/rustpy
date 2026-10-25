"""Provenance and fail-closed controls for the Linux native evidence producer."""
import hashlib
import importlib.util
import io
from pathlib import Path
import struct
import tarfile
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location('linux_native_evidence', ROOT / 'scripts/prepare-linux-native-evidence.py')
EVIDENCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EVIDENCE)


def elf(code=b'\xc3', debug=b'producer debug paths'):
    names = b'\0.text\0.debug_info\0.shstrtab\0'
    body = code + debug + names
    section_offset = (64 + len(body) + 7) & ~7
    ident = b'\x7fELF\x02\x01\x01' + b'\0' * 9
    header = struct.pack('<HHIQQQIHHHHHH', 3, 62, 1, 0x401000, 0, section_offset, 0,
                         64, 0, 0, 64, 4, 3)
    sections = [(0,) * 10,
                (1, 1, 6, 0x401000, 64, len(code), 0, 0, 1, 0),
                (7, 1, 0, 0, 64 + len(code), len(debug), 0, 0, 1, 0),
                (19, 3, 0, 0, 64 + len(code) + len(debug), len(names), 0, 0, 1, 0)]
    return ident + header + body + b'\0' * (section_offset - 64 - len(body)) + b''.join(
        struct.pack('<IIQQQQIIQQ', *section) for section in sections)


class LinuxNativeEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='linux-evidence-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def test_stripping_debug_sections_preserves_runtime_equivalence(self):
        linked = self.root / 'linked'
        packaged = self.root / 'packaged'
        linked.write_bytes(elf())
        packaged.write_bytes(elf(debug=b''))
        self.assertNotEqual(EVIDENCE.digest(linked), EVIDENCE.digest(packaged))
        self.assertEqual(EVIDENCE.allocated_sections(linked), EVIDENCE.allocated_sections(packaged))
        packaged.write_bytes(elf(code=b'\x90', debug=b''))
        self.assertNotEqual(EVIDENCE.allocated_sections(linked), EVIDENCE.allocated_sections(packaged))

    def test_link_record_authenticates_original_map_trace_and_runtime_bytes(self):
        linked = self.root / 'linked'
        packaged = self.root / 'packaged'
        link_map = self.root / 'link.map'
        trace = self.root / 'link.trace'
        linked.write_bytes(elf())
        packaged.write_bytes(elf(debug=b''))
        link_map.write_text('/usr/lib/crti.o:(.init)\n')
        trace.write_text('/usr/lib/crti.o\n')
        record = {'linked_binary_sha256': EVIDENCE.digest(linked),
                  'link_map_sha256': EVIDENCE.digest(link_map),
                  'link_trace_sha256': EVIDENCE.digest(trace), 'rustc_commit': 'a' * 40}
        EVIDENCE.validate_link_record(record, linked, packaged, link_map, trace, 'a' * 40)
        with self.assertRaisesRegex(ValueError, 'toolchain'):
            EVIDENCE.validate_link_record(record, linked, packaged, link_map, trace, 'b' * 40)
        link_map.write_text('/usr/lib/other.o:(.init)\n')
        with self.assertRaisesRegex(ValueError, 'link_map_sha256'):
            EVIDENCE.validate_link_record(record, linked, packaged, link_map, trace, 'a' * 40)

    def test_dsc_sha256_inventory_accepts_signed_metadata_and_rejects_traversal(self):
        sha = '1' * 64
        text = '-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA512\n\nFormat: 3.0 (quilt)\nChecksums-Sha256:\n ' + sha + ' 42 source.orig.tar.xz\nFiles:\n old digest\n'
        self.assertEqual(EVIDENCE.dsc_checksums(text), {'source.orig.tar.xz': (sha, 42)})
        with self.assertRaisesRegex(ValueError, 'Unsafe'):
            EVIDENCE.dsc_checksums(text.replace('source.orig.tar.xz', '../outside'))
        with self.assertRaisesRegex(ValueError, 'SHA256'):
            EVIDENCE.dsc_checksums(text.replace(sha, '1' * 63))

    def test_corrupt_source_download_never_becomes_an_evidence_file(self):
        data = b'actual source archive bytes'
        downloader = EVIDENCE.Downloads(opener=lambda *args, **kwargs: io.BytesIO(data))
        destination = self.root / 'source.tar.xz'
        with self.assertRaisesRegex(ValueError, 'SHA256'):
            downloader.file('https://source.example/archive', destination, '0' * 64)
        self.assertFalse(destination.exists())
        self.assertFalse(destination.with_name(destination.name + '.part').exists())
        downloader.file('https://source.example/archive', destination, hashlib.sha256(data).hexdigest(), len(data))
        self.assertEqual(destination.read_bytes(), data)

    def test_plain_http_source_downloads_are_refused(self):
        downloader = EVIDENCE.Downloads(opener=lambda *args, **kwargs: self.fail('HTTP must not be opened'))
        with self.assertRaisesRegex(ValueError, 'HTTPS'):
            downloader.file('http://source.example/archive', self.root / 'source')

    def test_link_map_paths_include_archive_members_and_shared_host_libraries(self):
        text = ' 12 12 /build/deps/libring-123.rlib(native.o):(.text)\n/usr/lib/libc.so.6\n'
        self.assertEqual(EVIDENCE.link_inputs(text), [Path('/build/deps/libring-123.rlib'), Path('/usr/lib/libc.so.6')])
        self.assertIn(self.root / 'unknown/native.a',
                      EVIDENCE.link_inputs('/usr/lib/crti.o\nunknown/native.a\n', self.root))
        with self.assertRaisesRegex(ValueError, 'no object'):
            EVIDENCE.link_inputs('no recorded inputs\n')

    def test_unknown_static_inputs_do_not_produce_an_empty_declaration(self):
        with patch.object(EVIDENCE, 'system_owner', side_effect=ValueError('unattributed static archive')):
            with self.assertRaisesRegex(ValueError, 'unattributed'):
                EVIDENCE.inventory([Path('/outside/native.a')], self.root, self.root / 'target', self.root / 'rust', [])
        with self.assertRaisesRegex(ValueError, 'Unattributed standalone'):
            EVIDENCE.inventory([self.root / 'target/unknown.rcgu.o'], self.root, self.root / 'target', self.root / 'rust', [])
        with self.assertRaisesRegex(ValueError, 'Unreviewed toolchain'):
            EVIDENCE.inventory([self.root / 'rust/libc.a'], self.root, self.root / 'target', self.root / 'rust', [])

    def test_cargo_archive_versions_are_resolved_from_dependency_provenance(self):
        archive = self.root / 'libexample-abc123.rlib'
        dependency = self.root / 'example-abc123.d'
        packages = [{'name': 'example', 'version': version,
                     'manifest_path': str(self.root / ('example-' + version) / 'Cargo.toml'),
                     'targets': [{'name': 'example'}]} for version in ('1.0.0', '2.0.0')]
        with self.assertRaisesRegex(ValueError, 'without dependency'):
            EVIDENCE.crate_owner(archive, packages)
        dependency.write_text('output: ' + str(self.root / 'example-2.0.0/src/lib.rs'))
        self.assertEqual(EVIDENCE.crate_owner(archive, packages)['version'], '2.0.0')

    def test_bundled_libraries_require_separate_runtime_evidence(self):
        binary = self.root / 'rustpy'
        binary.write_bytes(elf())
        EVIDENCE.validate_runtime(self.root, binary)
        (self.root / 'libextra.so.1').write_bytes(b'library')
        with self.assertRaisesRegex(ValueError, 'own evidence'):
            EVIDENCE.validate_runtime(self.root, binary)

    def test_native_crate_source_authentication_uses_the_lock_checksum(self):
        cargo = self.root / 'cargo'
        cache = cargo / 'registry/cache/index'
        cache.mkdir(parents=True)
        archive = cache / 'ring-1.0.0.crate'
        with tarfile.open(archive, 'w:gz') as source:
            text = b'Exact native crate license\n'
            info = tarfile.TarInfo('ring-1.0.0/LICENSE')
            info.size = len(text)
            source.addfile(info, io.BytesIO(text))
        sha = EVIDENCE.digest(archive)
        (self.root / 'Cargo.lock').write_text('[[package]]\nname = "ring"\nversion = "1.0.0"\nchecksum = "' + sha + '"\n')
        output = self.root / 'evidence'
        output.mkdir()
        result = EVIDENCE.cargo_component({'name': 'ring', 'version': '1.0.0'}, output, cargo, self.root)
        self.assertEqual(result['source_archives']['sources/ring-1.0.0.crate'], sha)
        self.assertEqual((output / result['license_files'][0]).read_bytes(), text)
        archive.write_bytes(b'tampered')
        with self.assertRaisesRegex(ValueError, 'unauthenticated'):
            EVIDENCE.cargo_component({'name': 'ring', 'version': '1.0.0'}, output, cargo, self.root)


if __name__ == '__main__':
    unittest.main()
