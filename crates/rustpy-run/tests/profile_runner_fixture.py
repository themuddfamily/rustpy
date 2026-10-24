"""Unexecuted vmprof conversion fixture, independent of native vmprof support."""
import importlib.util
import io
import json
import pathlib
import sys
import types
import unittest

source = pathlib.Path(__file__).parents[1] / 'src' / 'profile_runner.py'
spec = importlib.util.spec_from_file_location('rustpy_profile_fixture', source)
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)

class ProfileRunnerTests(unittest.TestCase):
    def test_vmprof_sample_weights_become_sample_metadata_and_real_edges(self):
        leaf = types.SimpleNamespace(name='py:worker:7:/project/app.py', count=4, self_count=4, children={})
        root = types.SimpleNamespace(name='py:main:1:/project/app.py', count=10, self_count=6, children={7: leaf})
        sampled = types.SimpleNamespace(get_tree=lambda: root, get_runtime_in_microseconds=lambda: 1000000)
        original = sys.modules.get('vmprof')
        sys.modules['vmprof'] = types.SimpleNamespace(read_profile=lambda path: sampled)
        try:
            stream = io.StringIO()
            runner.emit_vmprof('fixture.vmprof', stream)
            records = [json.loads(line.removeprefix(runner.PREFIX)) for line in stream.getvalue().splitlines()]
            worker = next(record for record in records if record.get('name') == 'worker')
            self.assertEqual(worker['callers'][0]['name'], 'main')
            self.assertAlmostEqual(worker['tottime'], 0.4)
            self.assertTrue(records[-1]['sampling'])
            self.assertEqual(records[-1]['engine'], 'vmprof')
        finally:
            if original is None:
                sys.modules.pop('vmprof', None)
            else:
                sys.modules['vmprof'] = original

if __name__ == '__main__':
    unittest.main()
