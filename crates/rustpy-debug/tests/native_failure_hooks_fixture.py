"""Unexecuted fixtures for native result adapters; no debugpy dependency."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('rustpy_native_failure_hooks', Path(__file__).parents[2] / 'rustpy-test' / 'src' / 'debug_failures.py')
hooks = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hooks)

class NativeFailureHooks(unittest.TestCase):
    def test_subtest_receives_real_exception_without_changing_result_signature(self):
        calls, results = [], []
        previous = hooks.debug_failure
        hooks.debug_failure = calls.append
        class Result:
            def addSubTest(self, test, subtest, error):
                results.append((test, subtest, error))
        try:
            hooks._wrap_result(Result, 'addSubTest')
            result = Result()
            result.addSubTest('test', 'subtest', None)
            info = (ValueError, ValueError('failed'), None)
            result.addSubTest('test', 'subtest', info)
            self.assertEqual(calls, [info])
            self.assertEqual(results, [('test', 'subtest', None), ('test', 'subtest', info)])
        finally:
            hooks.debug_failure = previous

    def test_step_wrapper_preserves_exception_and_user_locals(self):
        calls = []
        previous = hooks.debug_failure
        hooks.debug_failure = calls.append
        class Step:
            def run(self):
                user_local = 'retained'
                raise AssertionError(user_local)
        try:
            hooks._wrap_exception(Step, 'run')
            with self.assertRaisesRegex(AssertionError, 'retained'):
                Step().run()
            tb = calls[0][2]
            while tb.tb_next:
                tb = tb.tb_next
            self.assertEqual(tb.tb_frame.f_locals['user_local'], 'retained')
        finally:
            hooks.debug_failure = previous

if __name__ == '__main__':
    unittest.main()
