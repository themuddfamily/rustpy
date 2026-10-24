"""Regression fixtures; intentionally unexecuted during feature implementation."""
import importlib.util
import json
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('rustpy_debug_values', Path(__file__).parents[1] / 'src' / 'backend_values.py')
values = importlib.util.module_from_spec(spec)
spec.loader.exec_module(values)

class ValueViews(unittest.TestCase):
    def test_dictionary_paging_preserves_total_and_offset(self):
        table = json.loads(values.render({'first': 1, 'second': 2, 'third': 3}, 'table', {'offset': 1, 'rows': 1}))
        self.assertEqual(table['rows'], [["'second'", '2']])
        self.assertEqual(table['total'], 3)
        self.assertEqual(table['offset'], 1)

    def test_json_overflow_is_an_error_instead_of_invalid_truncated_json(self):
        with self.assertRaisesRegex(ValueError, 'budget'):
            values.render({'text': 'x' * 1048576}, 'json', {})

    def test_bytes_and_unknown_renderer(self):
        self.assertEqual(values.render(b'\x00\xff', 'bytes', {}), '00 ff')
        with self.assertRaisesRegex(ValueError, 'Unknown'):
            values.render(1, 'unregistered', {})

    def test_slice_rejects_code(self):
        with self.assertRaises(ValueError):
            values.render([1, 2], 'table', {'slice': '__import__("os")'})

if __name__ == '__main__':
    unittest.main()
