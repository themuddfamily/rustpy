"""Release packaging must carry project terms and source obligations together."""
import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location('stage_native_licenses', ROOT / 'packaging/stage-native-licenses.py')
STAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(STAGE)


class LicensePackagingTests(unittest.TestCase):
    def test_source_archive_and_native_evidence_travel_together(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            executable = root / 'rustpy.exe'
            executable.write_bytes(b'fixture')
            output = root / 'Native-Licenses.zip'
            calls = []

            def collect(command, **kwargs):
                calls.append(command)
                if '--output' in command:
                    destination = Path(command[command.index('--output') + 1])
                    destination.mkdir()
                    (destination / 'native-license.txt').write_text('retained terms')
                    runtime = Path(command[command.index('--runtime-root') + 1])
                    self.assertEqual((runtime / 'rustpy.exe').read_bytes(), b'fixture')
                    binary = Path(command[command.index('--binary') + 1])
                    self.assertEqual(binary.read_bytes(), b'fixture')
                else:
                    Path(command[command.index('--source-bundle') + 1]).write_bytes(b'exact sources')

            with patch.object(sys, 'argv', ['stage', '--runtime-file', str(executable), '--output', str(output)]), patch.object(STAGE.subprocess, 'run', side_effect=collect), patch.dict(STAGE.os.environ, {'TMPDIR': str(root)}, clear=True):
                STAGE.main()
            self.assertEqual(len(calls), 2)
            with zipfile.ZipFile(output) as archive:
                self.assertEqual(archive.read('Native-Licenses/native-license.txt'), b'retained terms')
                self.assertEqual(archive.read('Native-Licenses/cargo-covered-sources.tar.gz'), b'exact sources')

    def test_collection_failure_does_not_replace_reviewed_bundle(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / 'Native-Licenses.zip'
            output.write_bytes(b'existing reviewed bundle')
            with patch.object(sys, 'argv', ['stage', '--binary', str(output), '--runtime-root', str(root), '--output', str(output)]), patch.object(STAGE.subprocess, 'run', side_effect=subprocess.CalledProcessError(1, 'collector')), patch.dict(STAGE.os.environ, {'TMPDIR': str(root)}, clear=True):
                with self.assertRaises(subprocess.CalledProcessError):
                    STAGE.main()
            self.assertEqual(output.read_bytes(), b'existing reviewed bundle')

    def test_injected_runtime_requires_explicit_manifest_coverage(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / 'rustpy'
            binary.write_bytes(b'fixture')
            manifest = root / 'manifest.json'
            manifest.write_text('{"components": []}')
            arguments = ['stage', '--runtime-root', str(root), '--binary', str(binary),
                         '--required-native-file', 'usr/lib/rustpy/appimage-runtime',
                         '--output', str(root / 'Native-Licenses.zip')]
            with patch.object(sys, 'argv', arguments), patch.object(STAGE.subprocess, 'run') as collect, patch.dict(STAGE.os.environ, {'TMPDIR': str(root), 'RUSTPY_NATIVE_RUNTIME_MANIFEST': str(manifest)}, clear=True):
                with self.assertRaises(SystemExit):
                    STAGE.main()
                collect.assert_not_called()

    def test_appimage_injects_the_reviewed_retained_runtime(self):
        source = (ROOT / 'packaging/linux/build-appimage.sh').read_text()
        self.assertIn('--runtime-file "$APPDIR/$RUNTIME_REL"', source)
        self.assertIn('--required-native-file "$RUNTIME_REL"', source)
        self.assertLess(source.index('install -Dm755 "$APPIMAGETOOL_RUNTIME_FILE"'), source.index('stage-native-licenses.py'))
        macos = (ROOT / 'packaging/macos/build-app.sh').read_text()
        self.assertIn('--binary-stage pre-sign', macos)

    def test_native_package_hash_gate_runs_after_staging_strip(self):
        for recipe in ['packaging/linux/build-deb.sh', 'packaging/linux/build-rpm.sh']:
            source = (ROOT / recipe).read_text()
            with self.subTest(recipe=recipe):
                self.assertLess(source.index('"$STRIP_TOOL" --strip-'), source.index('stage-native-licenses.py --binary'))
                self.assertIn('--binary "$STAGE_BIN_DIR/rustpy"', source)
        deb = (ROOT / 'packaging/linux/build-deb.sh').read_text()
        self.assertIn('--locked --no-build --no-strip', deb)
        self.assertIn('CARGO_TARGET_DIR="$STAGE" cargo deb', deb)

    def test_installers_require_license_and_source_archive(self):
        for recipe in ['packaging/windows/rustpy.iss', 'packaging/windows/rustpy.wxs', 'crates/rustpy/Cargo.toml']:
            source = (ROOT / recipe).read_text()
            with self.subTest(recipe=recipe):
                self.assertIn('THIRD-PARTY-NOTICES.txt', source)
                self.assertIn('LICENSE', source)
                self.assertIn('Native-Licenses', source)


if __name__ == '__main__':
    unittest.main()
