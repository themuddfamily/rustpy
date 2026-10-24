"""Exercise the reporter adapter without starting debugpy or executing tests."""
import ast
import os
from pathlib import Path
import sys
import types
import unittest
from unittest.mock import patch

class PostmortemFrames(unittest.TestCase):
    def test_failed_frame_and_exception_identity_reach_suspension(self):
        reporter = Path(__file__).parents[2] / 'rustpy-test' / 'src' / 'reporter.py'
        tree = ast.parse(reporter.read_text(encoding='utf-8'))
        function = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == 'debug_failure')
        events, calls = [], []
        debugger = types.SimpleNamespace(set_suspend=lambda *args: calls.append(('suspend', args)), do_wait_suspend=lambda *args: calls.append(('wait', args)))
        constants = types.ModuleType('_pydevd_bundle.pydevd_constants')
        constants.CMD_ADD_EXCEPTION_BREAK = 122
        constants.get_global_debugger = lambda: debugger
        namespace = {'os': os, 'emit': lambda **event: events.append(event)}
        exec(compile(ast.Module(body=[function], type_ignores=[]), str(reporter), 'exec'), namespace)
        def failing():
            retained_local = 42
            raise ValueError(retained_local)
        try:
            failing()
        except ValueError:
            exception = sys.exc_info()
        with patch.dict(os.environ, {'RUSTPY_DEBUG_FAILED_TESTS': '1'}), patch.dict(sys.modules, {'debugpy': types.SimpleNamespace(is_client_connected=lambda: True), '_pydevd_bundle': types.ModuleType('_pydevd_bundle'), '_pydevd_bundle.pydevd_constants': constants}):
            namespace['debug_failure'](exception)
        self.assertEqual([call[0] for call in calls], ['suspend', 'wait'])
        self.assertEqual(calls[1][1][1].f_locals['retained_local'], 42)
        self.assertIs(calls[1][1][3], exception)
        self.assertEqual(events, [])

if __name__ == '__main__':
    unittest.main()
