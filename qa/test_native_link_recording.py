"""Exercise linker evidence creation with a small stand-in driver, not a compiler."""
import hashlib
import importlib.util
import json
import ctypes
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location('native_link_recording', ROOT / 'scripts/record-native-link.py')
RECORD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RECORD)


class NativeLinkRecording(unittest.TestCase):
    def test_windows_response_lines_preserve_quoted_paths_and_expose_output_argument(self):
        buffers = []
        freed = []

        def command_line_to_argv(command, count):
            # Model the WinAPI distinction that caused the failure: only space
            # and tab separate arguments. A raw LF remains part of the token.
            lexer = shlex.shlex(command, posix=False)
            lexer.whitespace = ' \t'
            lexer.whitespace_split = True
            words = [word[1:-1] if word.startswith('"') and word.endswith('"') else word
                     for word in lexer]
            ctypes.cast(count, ctypes.POINTER(ctypes.c_int)).contents.value = len(words)
            array = (ctypes.c_wchar_p * len(words))(*words)
            buffers.append(array)
            return array

        def local_free(pointer):
            freed.append(pointer)

        api = SimpleNamespace(shell32=SimpleNamespace(CommandLineToArgvW=command_line_to_argv),
                              kernel32=SimpleNamespace(LocalFree=local_free))
        text = '/NOLOGO\r\n' + r'"C:\Program Files (x86)\SDK\libucrt.lib"' + '\n/OUT:C:/build/rustpy.exe\r\n'
        with patch.object(RECORD.os, 'name', 'nt'), patch.object(RECORD.ctypes, 'windll', api, create=True):
            arguments = RECORD.response_arguments(text)
        self.assertEqual(arguments, ['/NOLOGO', r'C:\Program Files (x86)\SDK\libucrt.lib', '/OUT:C:/build/rustpy.exe'])
        self.assertEqual(RECORD.linked_output(arguments), Path('C:/build/rustpy.exe'))
        self.assertEqual(len(freed), 1)

    def test_success_records_actual_output_trace_and_expanded_command(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / 'native binary'
            link_map = root / 'link map'
            proof = root / 'proof'
            program = ('from pathlib import Path; import sys; '
                       'Path(sys.argv[2]).write_bytes(b"linked binary"); '
                       'Path(sys.argv[3]).write_text("native input map"); '
                       'print("selected input library")')
            result = subprocess.run([
                sys.executable, '-B', str(ROOT / 'scripts/record-native-link.py'),
                '--driver', sys.executable, '--directory', str(proof),
                '--link-map', str(link_map), '--rustc-commit', 'a' * 40,
                '--', '-c', program, '-o', str(binary), str(link_map),
            ], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            record = json.loads((proof / 'link-record.json').read_text())
            self.assertEqual(record['binary_sha256'], hashlib.sha256(b'linked binary').hexdigest())
            self.assertEqual(record['link_map_sha256'], RECORD.digest(link_map))
            self.assertEqual(record['link_trace_sha256'], RECORD.digest(proof / 'link-trace.txt'))
            self.assertEqual(record['expanded_arguments'][-3:], ['-o', str(binary), str(link_map)])

    def test_driver_failure_preserves_status_and_removes_stale_success(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'link-record.json').write_text('stale')
            result = subprocess.run([
                sys.executable, '-B', str(ROOT / 'scripts/record-native-link.py'),
                '--driver', sys.executable, '--directory', str(root),
                '--link-map', str(root / 'absent.map'), '--rustc-commit', 'a' * 40,
                '--', '-c', 'import sys; print("link failed"); sys.exit(42)',
            ], capture_output=True, text=True)
            self.assertEqual(result.returncode, 42)
            self.assertFalse((root / 'link-record.json').exists())
            self.assertIn('link failed', (root / 'link-trace.txt').read_text())

    def test_response_bytes_are_retained_and_cycles_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            response = root / 'input.rsp'
            response.write_text('-o "binary with spaces" input.lib', encoding='utf-16')
            records = []
            expanded = RECORD.retain_arguments(['@' + str(response)], root, records)
            self.assertEqual(expanded, ['-o', 'binary with spaces', 'input.lib'])
            self.assertEqual((root / records[0]['retained']).read_bytes(), response.read_bytes())
            self.assertEqual(records[0]['sha256'], RECORD.digest(response))
            response.write_text('@' + str(response))
            with self.assertRaisesRegex(ValueError, 'Recursive'):
                RECORD.retain_arguments(['@' + str(response)], root, [])

    def test_output_location_is_taken_from_linker_options(self):
        self.assertEqual(RECORD.linked_output(['/OUT:C:/release/rustpy.exe']), Path('C:/release/rustpy.exe'))
        self.assertEqual(RECORD.linked_output(['-o', '/release/rustpy']), Path('/release/rustpy'))
        self.assertIsNone(RECORD.linked_output(['/help']))

    def test_temporary_import_library_survives_compiler_cleanup(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / 'rustc-fixture' / 'kernel32.dll.lib'
            source.parent.mkdir()
            source.write_bytes(b'generated import library')
            proof = root / 'proof'
            proof.mkdir()
            with patch.dict(RECORD.os.environ, {'TMPDIR': str(root)}):
                records = RECORD.retain_native_inputs([str(source)], f'Loaded {source}(import.obj)', proof)
            self.assertEqual(len(records), 1)
            self.assertEqual(records[0]['sha256'], RECORD.digest(source))
            source.unlink()
            self.assertEqual((proof / records[0]['retained']).read_bytes(), b'generated import library')


if __name__ == '__main__':
    unittest.main()
