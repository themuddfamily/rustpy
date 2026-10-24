"""Authored, unexecuted regression checks for source-only quality infrastructure."""
import importlib.util
from pathlib import Path
import unittest
ROOT = Path(__file__).resolve().parent.parent

def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

class QualityRecipes(unittest.TestCase):
    def test_automation_is_explicit_and_unconditionally_disabled(self):
        for name in ("ci.yml", "release.yml"):
            text = (ROOT / '.github/workflows' / name).read_text()
            self.assertNotIn('\n  push:', text)
            self.assertNotIn('\n  pull_request:', text)
            self.assertIn('workflow_dispatch:', text)
            jobs = text.split('\njobs:\n', 1)[1]
            blocks = []
            for line in jobs.splitlines():
                if line.startswith('  ') and not line.startswith('   ') and line.endswith(':'):
                    blocks.append([])
                if blocks:
                    blocks[-1].append(line)
            self.assertTrue(blocks)
            for block in blocks:
                self.assertIn('    if: ${{ false }}', block)

    def test_expensive_gates_are_explicit_and_fuzz_is_bounded(self):
        recipe = load('quality_recipe', ROOT / 'scripts/quality.py')
        self.assertIn('bench', recipe.STAGES)
        self.assertIn('fuzz', recipe.STAGES)
        self.assertIn('-jobs=1', recipe.STAGES['fuzz'])
        self.assertIn('-max_total_time=60', recipe.STAGES['fuzz'])
        for command in recipe.STAGES.values():
            self.assertNotIn('install', command)
            self.assertNotIn('publish', command)
            # Dependency-resolving Cargo gates must honor their reviewed lock;
            # offline mode alone still permits rewriting a local lockfile.
            if command[:2] in (['cargo', 'test'], ['cargo', 'build'],
                               ['cargo', 'clippy'], ['cargo', 'bench']):
                cargo_arguments = command[:command.index('--')] if '--' in command else command
                self.assertIn('--locked', cargo_arguments)
        # Standalone QA gates must keep their own manifest and intended scope:
        # the normal QA suite excludes the explicitly requested ignored corpus.
        for stage in ('qa', 'corpus', 'bench'):
            command = recipe.STAGES[stage]
            self.assertEqual(command[command.index('--manifest-path') + 1], 'qa/Cargo.toml')
        self.assertNotIn('--ignored', recipe.STAGES['qa'])
        self.assertIn('--test-threads=2', recipe.STAGES['qa'])
        self.assertIn('--ignored', recipe.STAGES['corpus'])
        self.assertIn('--test-threads=1', recipe.STAGES['corpus'])
        self.assertEqual(recipe.STAGES['corpus'][recipe.STAGES['corpus'].index('--test') + 1], 'parser_sources')
        self.assertEqual(recipe.STAGES['bench'][recipe.STAGES['bench'].index('--bench') + 1], 'editor_pipeline')

    def test_keymap_generation_retains_unbound_and_alternative_bindings(self):
        generator = load('keymap_reference', ROOT / 'scripts/update-keymap-reference.py')
        rendered = generator.render('''
        Save => ("SaveDocument", "Save", ["Ctrl+S", "Meta+S"]),
        Unbound => ("Extra", "Extra Action", []),
        ''')
        self.assertIn('Ctrl+S; Meta+S', rendered)
        self.assertIn('| `Extra` | Unbound |', rendered)
        with self.assertRaises(ValueError):
            generator.render('invalid schema')


class QualityAdmissionContracts(unittest.TestCase):
    """UNEXECUTED: isolated source fixtures; never invoke Cargo or tools."""
    def test_output_root_defaults_to_system_temp_and_requires_existing_dir(self):
        import tempfile
        from unittest.mock import patch
        recipe = load('quality_admission', ROOT / 'scripts/quality.py')
        self.assertEqual(recipe.output_root(''), Path(tempfile.gettempdir()))
        with patch.object(Path, 'is_dir', return_value=False):
            with self.assertRaises(ValueError): recipe.output_root('/no/such/dir')
        with patch.object(Path, 'is_dir', return_value=True):
            self.assertEqual(recipe.output_root('/tmp/quality-out'), Path('/tmp/quality-out').resolve())

    def test_each_declared_corpus_root_must_contribute_inputs(self):
        from unittest.mock import patch
        recipe = load('quality_corpus_admission', ROOT / 'scripts/quality.py')
        with patch.object(Path,'is_dir',return_value=True), patch.object(Path,'is_symlink',return_value=False), patch.object(Path,'is_file',return_value=True):
            with patch.object(recipe.os,'walk',side_effect=[[('/fixture/one',[],['valid.py'])],[('/fixture/empty',[],['readme.txt'])]]):
                with self.assertRaises(ValueError): recipe.admitted_corpus('/fixture/one:/fixture/empty')
            with patch.object(recipe.os,'walk',return_value=[('/fixture/stubs',[],['module.pyi'])]):
                with self.assertRaises(ValueError): recipe.admitted_corpus('/fixture/stubs',('.py',))
            with self.assertRaises(ValueError): recipe.admitted_corpus(':/fixture/one')
        with patch.object(Path,'is_dir',return_value=True), patch.object(Path,'is_symlink',return_value=False), patch.object(Path,'is_file',return_value=False), patch.object(recipe.os,'walk',return_value=[('/fixture',[],['pipe.py'])]):
            with self.assertRaises(ValueError): recipe.admitted_corpus('/fixture')

    def test_fuzz_corpus_and_artifacts_are_isolated_from_tracked_seeds(self):
        from unittest.mock import patch
        recipe = load('quality_fuzz_admission', ROOT / 'scripts/quality.py')
        with patch.object(Path,'mkdir'):
            command = recipe.fuzz_arguments(recipe.STAGES['fuzz'],Path('/tmp/rustpy-root/final/gate'))
        first = command[command.index('--')+1]
        second = command[command.index('--')+2]
        self.assertEqual(first,'/tmp/rustpy-root/final/gate/fuzz-corpus')
        self.assertEqual(second,str(ROOT/'qa/fuzz/corpus/python'))
        self.assertIn('-artifact_prefix=/tmp/rustpy-root/final/gate/fuzz-artifacts/',command)

    def test_offline_app_paths_and_caps_override_real_user_environment(self):
        from unittest.mock import patch
        recipe = load('quality_environment', ROOT / 'scripts/quality.py')
        before=dict(RUSTPY_CONFIG_DIR='/root/real-profile',CARGO_TARGET_DIR='/existing/shared-target',RAYON_NUM_THREADS='64',RUSTPY_TRUST_ALL='1')
        with patch.object(Path,'mkdir'):
            after=recipe.gate_environment(before,Path('/tmp/rustpy-root/final/gate'))
        self.assertEqual(after['RUSTPY_CONFIG_DIR'],'/tmp/rustpy-root/final/gate/config')
        self.assertEqual(after['CARGO_TARGET_DIR'],before['CARGO_TARGET_DIR'])
        self.assertEqual(after['CARGO_NET_OFFLINE'],'true')
        self.assertEqual(after['RUSTUP_AUTO_INSTALL'],'0')
        self.assertNotIn('RUSTPY_TRUST_ALL',after)
        for key in ('CARGO_BUILD_JOBS','RAYON_NUM_THREADS','RUST_TEST_THREADS'):
            self.assertEqual(after[key],'2')
        self.assertEqual(before['RAYON_NUM_THREADS'],'64')
        deny = recipe.STAGES['deny']
        self.assertEqual(deny[:2], ['cargo', 'deny'])
        self.assertIn('--frozen', deny)
        self.assertLess(deny.index('--frozen'), deny.index('check'))
        self.assertEqual(deny[deny.index('--config') + 1], 'qa/deny.toml')

if __name__ == '__main__':
    unittest.main()
