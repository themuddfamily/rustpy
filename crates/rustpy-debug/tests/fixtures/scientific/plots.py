"""Unexecuted capture regression fixtures; no GUI, subprocess or network needed."""
import base64
import builtins
import importlib.util
import os
from pathlib import Path
import sys
import types
import unittest
SOURCE = Path(__file__).parents[3] / 'src' / 'scientific_plots.py'
spec = importlib.util.spec_from_file_location('scientific_plots_fixture', SOURCE)
plots = importlib.util.module_from_spec(spec)
spec.loader.exec_module(plots)

class CaptureFixtures(unittest.TestCase):
    def setUp(self):
        self.original_environment={key:os.environ.get(key) for key in ["RUSTPY_SCIENTIFIC_OUTPUT","RUSTPY_SCIENTIFIC_INTERACTIVE"]}
        os.environ.pop("RUSTPY_SCIENTIFIC_OUTPUT",None)
        os.environ["RUSTPY_SCIENTIFIC_INTERACTIVE"]="1"
        self.original_import = builtins.__import__
        self.original_state = getattr(builtins, '_rustpy_scientific', None)
        if hasattr(builtins, '_rustpy_scientific'):
            del builtins._rustpy_scientific
        self.original_modules = {key: sys.modules.get(key) for key in ['matplotlib.pyplot', 'plotly.basedatatypes']}

    def tearDown(self):
        for key,value in self.original_environment.items():
            if value is None:
                os.environ.pop(key,None)
            else:
                os.environ[key]=value
        builtins.__import__ = self.original_import
        if self.original_state is None:
            del builtins._rustpy_scientific
        else:
            builtins._rustpy_scientific = self.original_state
        for key, value in self.original_modules.items():
            if value is None:
                sys.modules.pop(key, None)
            else:
                sys.modules[key] = value

    def test_agg_capture_deduplicates_unchanged_figure_and_keeps_it_live(self):
        class Figure:
            version = 1
            def get_figwidth(self): return 2
            def get_figheight(self): return 1
            def savefig(self, stream, **kwargs): stream.write(('PNG-%s' % self.version).encode())
        figure = Figure()
        fake = types.SimpleNamespace(get_fignums=lambda: [1], figure=lambda number: figure, show=lambda: None)
        sys.modules['matplotlib.pyplot'] = fake
        plots.scientific_install(True)
        first = plots.scientific_capture()
        self.assertEqual(base64.b64decode(first[0]['data_base64']), b'PNG-1')
        self.assertEqual(plots.scientific_capture(), [])
        figure.version = 2
        self.assertEqual(len(plots.scientific_capture()), 1)
        plots.scientific_install(False)
        self.assertEqual(plots.scientific_capture(), [])

    def test_plotly_show_retains_self_contained_html_without_opening_browser(self):
        class Figure:
            def show(self, *args, **kwargs): raise AssertionError('Native browser should not open')
            def to_html(self, **kwargs):
                assert kwargs['include_plotlyjs'] is True
                return '<html>local plot data and assets</html>'
        sys.modules['plotly.basedatatypes'] = types.SimpleNamespace(BaseFigure=Figure)
        plots.scientific_install(True)
        plots.scientific_patch_interactive()
        Figure().show()
        output = plots.scientific_capture()
        self.assertEqual(output[0]['mime'], 'text/html')
        self.assertIn(b'local plot data', base64.b64decode(output[0]['data_base64']))

if __name__ == '__main__':
    unittest.main()
