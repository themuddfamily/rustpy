"""Unexecuted feature fixtures; run in the final verification stage."""
import importlib.util
import pathlib
import unittest

SOURCE = pathlib.Path(__file__).parents[1] / 'src' / 'console_values.py'
SPEC = importlib.util.spec_from_file_location('rustpy_console_values_fixture', SOURCE)
engine = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(engine)


class ValueEngineTests(unittest.TestCase):
    def test_debugger_frame_execution_uses_separate_globals_and_locals(self):
        global_namespace = {'shared': 10}
        local_namespace = {'local': 5}
        result = engine.execute('local += shared\nlocal', global_namespace, local_namespace)
        self.assertEqual(result, 15)
        self.assertEqual(local_namespace['local'], 15)
        self.assertNotIn('local', global_namespace)

    def test_hostile_repr_and_unrepresentable_dictionary_key(self):
        class Hostile:
            def __repr__(self):
                raise KeyboardInterrupt()
        key = Hostile()
        values = engine.Values({'mapping': {key: [1, 2]}})
        page = values.page('mapping', 0, 10)
        handle = page['items'][0]['expr']
        self.assertEqual(values.page(handle, 1, 1)['items'][0]['value'], '2')
        values.set_value(values.page(handle, 1, 1)['items'][0]['expr'], '3')
        self.assertEqual(values.namespace['mapping'][key], [1, 3])
        with self.assertRaises(ValueError):
            values.resolve(handle)

    def test_numpy_shape_summary_and_bounded_rectangle(self):
        try:
            import numpy as np
        except ImportError:
            self.skipTest('numpy is optional')
        value = np.arange(12000).reshape(120, 100)
        values = engine.Values({'array': value})
        self.assertIn('shape=(120, 100)', values.info('array', value)['value'])
        page = values.data('array', 10, 200, 20, 50)
        self.assertEqual((page['total_rows'], page['total_cols']), (120, 100))
        self.assertLessEqual(sum(map(len, page['rows'])), 2000)
        self.assertEqual(page['rows'][0][0], repr(value[10, 20]))

    def test_dataframe_summary_preserves_column_labels_and_index(self):
        try:
            import pandas as pd
        except ImportError:
            self.skipTest('pandas is optional')
        frame = pd.DataFrame({'alpha': [1, 2], 'beta': [3, 4]}, index=['first', 'second'])
        values = engine.Values({'frame': frame})
        self.assertEqual(values.info('frame', frame)['value'], 'DataFrame 2 × 2')
        page = values.data('frame', 1, 1, 1, 1)
        self.assertEqual(page['columns'], ['beta'])
        self.assertEqual(page['index'], ["'second'"])
        self.assertEqual(page['rows'], [[repr(frame.iloc[1, 1])]])


if __name__ == '__main__':
    unittest.main()
