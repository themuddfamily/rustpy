"""Source-only fixture proposal; mocked loader, genuine CPython descriptor objects."""
import contextlib
import io
import json
import pathlib
import os
import sys
import sysconfig
import tempfile
import types
import unittest
from unittest.mock import patch

class DescriptorTests(unittest.TestCase):
    def test_native_classmethod_is_callable_and_unavailable_signature_is_explicit(self):
        namespace = {'__name__': 'producer_fixture'}
        source_path = pathlib.Path(os.environ['RUSTPY_INTROSPECT_SOURCE']) if 'RUSTPY_INTROSPECT_SOURCE' in os.environ else pathlib.Path(__file__).resolve().parents[1] / 'src' / 'introspect.py'
        source = source_path.read_text()
        exec(compile(source, 'introspect.py', 'exec'), namespace)
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory).resolve()
            extension = root / 'fixture.so'
            extension.write_bytes(b'owned mocked loader input')
            module = types.ModuleType('fixture')
            native = type('Native', (), {'__module__': 'fixture', 'fromkeys': vars(dict)['fromkeys'],
                                        'real': vars(complex)['real']})
            module.Native = native
            interpreter = str(pathlib.Path(sys.executable).resolve())
            request = dict(explicitly_trusted=True, module_name='fixture', module_path=str(extension),
                           library_root=str(root), interpreter=interpreter, interpreter_sha256='a'*64,
                           module_sha256='a'*64, expected_version='.'.join(map(str, sys.version_info[:3])),
                           expected_cache_tag=sys.implementation.cache_tag,
                           expected_soabi=sysconfig.get_config_var('SOABI') or '')
            loader = types.SimpleNamespace(exec_module=lambda value: None)
            spec = types.SimpleNamespace(loader=loader)
            output = io.StringIO()
            original_signature = namespace['inspect'].signature
            def signature(value, **kwargs):
                if value is vars(dict)['fromkeys']:
                    raise ValueError('fixture unavailable')
                return original_signature(value, **kwargs)
            with patch.object(sys, 'argv', ['fixture', json.dumps(request)]), \
                 patch.dict(namespace, digest=lambda path: 'a'*64), \
                 patch.object(namespace['importlib'].util, 'spec_from_file_location', return_value=spec), \
                 patch.object(namespace['importlib'].util, 'module_from_spec', return_value=module), \
                 patch.object(namespace['inspect'], 'signature', side_effect=signature), \
                 patch.dict(sys.modules), contextlib.redirect_stdout(output):
                namespace['main']()
            artifact = json.loads(output.getvalue())
            self.assertIn('@classmethod\n    def fromkeys(cls: Any, *args: Any, **kwargs: Any) -> Any: ...', artifact['pyi'])
            self.assertNotIn('fromkeys: Any', artifact['pyi'])
            self.assertIn('signature unavailable: fromkeys', artifact['unsupported'])
            self.assertIn('descriptor type unavailable: real', artifact['unsupported'])

if __name__ == '__main__':
    unittest.main()
