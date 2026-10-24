"""Unexecuted regression fixtures for the shared interpreter-side adapters.
Run under the centrally capped final verification stage, not feature work.
"""
import importlib.util
from pathlib import Path
import unittest
SOURCE = Path(__file__).parents[3] / 'src'
spec = importlib.util.spec_from_file_location('scientific_table', SOURCE / 'scientific_table.py')
adapter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(adapter)

class AdapterFixtures(unittest.TestCase):
    def test_page_statistics_and_missing_values(self):
        page = adapter.scientific_table([{'value': 1}, {'value': 3}, {'value': None}], {'rows': 3})
        self.assertEqual(page['columns'], ['value'])
        self.assertEqual(page['stats'][0]['mean'], 2)
        self.assertEqual(page['stats'][0]['missing'], 1)
        self.assertEqual(page['stats'][0]['scope'], 'loaded page')

    def test_dictionary_offset_does_not_copy_unrequested_entries(self):
        page = adapter.scientific_table(dict((str(i), i) for i in range(20)), {'offset': 15, 'rows': 2})
        self.assertEqual(page['rows'], [['15', '15'], ['16', '16']])
        self.assertEqual(page['total_rows'], 20)

    def test_wide_rectangles_are_bounded(self):
        page = adapter.scientific_table([[i for i in range(1000)] for _ in range(100)], {'rows': 1000000, 'columns': 1000000, 'col_start': 900})
        self.assertLessEqual(sum(map(len, page['rows'])), 10000)
        self.assertEqual(len(page['columns']), 100)
        self.assertEqual(page['columns'][0], '900')

    def test_numpy_slices_preserve_original_shape_and_reject_implicit_flatten(self):
        try:
            import numpy as np
        except ImportError:
            self.skipTest('numpy not installed')
        array = np.arange(24).reshape(2, 3, 4)
        with self.assertRaisesRegex(ValueError, 'explicit slice'):
            adapter.scientific_table(array)
        page = adapter.scientific_table(array, {'slice': '1, :, 1:3'})
        self.assertEqual(page['shape'], [2, 3, 4])
        self.assertEqual(page['rows'][0], ['13', '14'])

    def test_series_and_polars_keep_names(self):
        try:
            import pandas as pd
            import polars as pl
        except ImportError:
            self.skipTest('pandas/polars not installed')
        series = adapter.scientific_table(pd.Series([1, 2], name='amount'))
        self.assertEqual(series['columns'], ['amount'])
        frame = adapter.scientific_table(pl.DataFrame({'amount': [1, 2]}))
        self.assertEqual(frame['stats'][0]['mean'], 1.5)

    def test_tensor_preview_never_converts_the_complete_accelerator_array(self):
        class Scalar:
            def detach(self): return self
            def cpu(self): return self
            def item(self): return 5
        class Tensor:
            __module__ = 'torch.fixture'
            shape = (1000000, 1000000)
            dtype = 'float32'
            def __getitem__(self, key): return Scalar()
            def numpy(self): raise AssertionError('Full tensor conversion is forbidden')
        page = adapter.scientific_table(Tensor(), {'rows': 2, 'columns': 2})
        self.assertEqual(page['rows'], [['5', '5'], ['5', '5']])
        self.assertEqual(page['total_cols'], 1000000)

if __name__ == '__main__':
    unittest.main()
