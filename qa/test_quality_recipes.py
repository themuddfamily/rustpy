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
    def test_unit_gate_requires_the_same_latency_case_in_an_isolated_phase(self):
        recipe = load('quality_unit_plan', ROOT / 'scripts/quality.py')
        original = list(recipe.STAGES['unit'])
        normal, *isolated_phases = recipe.stage_commands('unit')
        self.assertEqual(recipe.STAGES['unit'], original)
        self.assertEqual(normal[-4:], [item for test in recipe.SERIAL_UNIT_TESTS for item in ('--skip', test)])
        self.assertIn('--test-threads=2', normal)
        cargo_arguments = original[:original.index('--')]
        for test, isolated in zip(recipe.SERIAL_UNIT_TESTS, isolated_phases, strict=True):
            self.assertEqual(isolated[:len(cargo_arguments)], cargo_arguments)
            self.assertEqual(isolated[len(cargo_arguments):],
                             [test, '--', '--exact', '--test-threads=1'])
            self.assertNotIn('--skip', isolated)

    def test_unit_gate_runs_both_phases_and_propagates_either_failure(self):
        from types import SimpleNamespace
        from unittest.mock import call, patch
        recipe = load('quality_unit_execution', ROOT / 'scripts/quality.py')
        commands = recipe.stage_commands('unit')
        environment = {'CARGO_NET_OFFLINE': 'true', 'RAYON_NUM_THREADS': '2'}
        for first, second, third, expected in ((0, 0, 0, 0), (101, 0, 0, 101), (0, 101, 0, 101),
                                               (101, 1, 0, 101), (-9, 0, 0, 137), (0, 0, 101, 101)):
            with self.subTest(first=first, second=second):
                with patch.object(recipe.subprocess, 'run', side_effect=[
                    SimpleNamespace(returncode=first), SimpleNamespace(returncode=second), SimpleNamespace(returncode=third),
                ]) as run, patch('builtins.print'):
                    self.assertEqual(recipe.execute_commands(commands, environment), expected)
                self.assertEqual(run.call_args_list, [
                    call(command, env=environment, check=False) for command in commands
                ])

    def test_unit_gate_still_attempts_the_serial_phase_after_a_launch_error(self):
        from types import SimpleNamespace
        from unittest.mock import patch
        recipe = load('quality_unit_launch_error', ROOT / 'scripts/quality.py')
        commands = recipe.stage_commands('unit')
        with patch.object(recipe.subprocess, 'run', side_effect=[
            OSError('cannot start first phase'), SimpleNamespace(returncode=0), SimpleNamespace(returncode=0),
        ]) as run, patch('builtins.print'):
            self.assertEqual(recipe.execute_commands(commands, {}), 127)
        self.assertEqual(run.call_count, 3)
        self.assertEqual(run.call_args.args[0], commands[2])

    def test_unit_print_only_shows_both_phases_without_running_tools(self):
        from unittest.mock import patch
        recipe = load('quality_unit_print', ROOT / 'scripts/quality.py')
        with patch.object(recipe.sys, 'argv', ['quality.py', 'unit']), \
             patch('builtins.print') as printed, \
             patch.object(recipe.subprocess, 'run') as run, \
             patch.object(recipe.os, 'execvpe') as execute:
            self.assertEqual(recipe.main(), 0)
        self.assertEqual([item.args[0] for item in printed.call_args_list],
                         [recipe.shlex.join(command) for command in recipe.stage_commands('unit')])
        run.assert_not_called()
        execute.assert_not_called()

    def test_ci_runs_the_strict_latency_case_as_a_required_serial_step(self):
        recipe = load('quality_ci_latency', ROOT / 'scripts/quality.py')
        text = (ROOT / '.github/workflows/ci.yml').read_text()
        ordinary = text.split('      - name: UI tests\n', 1)[1].split('      - name:', 1)[0]
        isolated = text.split('      - name: Cancellation latency (isolated)\n', 1)[1].split('      - if:', 1)[0]
        self.assertIn("if: matrix.shard == 'ui'", ordinary)
        self.assertIn('--skip ' + recipe.SERIAL_UNIT_TEST, ordinary)
        self.assertIn("if: matrix.shard == 'ui'", isolated)
        self.assertIn(recipe.SERIAL_UNIT_TEST + ' -- --exact --test-threads=1', isolated)
        self.assertNotIn('continue-on-error', isolated)
        self.assertNotIn('--ignored', isolated)

    def test_ci_checks_pushes_and_pull_requests_without_publication(self):
        text = (ROOT / '.github/workflows/ci.yml').read_text()
        trigger = text.split('\non:\n', 1)[1].split('\npermissions:\n', 1)[0]
        self.assertIn('  push:\n    branches: [main]\n', trigger)
        self.assertIn('  pull_request:\n', trigger)
        self.assertIn('  workflow_dispatch:', trigger.splitlines())
        self.assertIn('\npermissions:\n  contents: read\n', text)
        self.assertNotIn('contents: write', text)
        for command in ('gh release ', 'git push ', 'cargo publish', 'actions/create-release'):
            self.assertNotIn(command, text)

    def test_release_publication_requires_explicit_manual_input(self):
        text = (ROOT / '.github/workflows/release.yml').read_text()
        trigger = text.split('\non:\n', 1)[1].split('\npermissions:\n', 1)[0]
        self.assertNotIn('\n  push:', trigger)
        self.assertNotIn('\n  pull_request:', trigger)
        self.assertIn('  workflow_dispatch:\n    inputs:\n', trigger)
        for name in ('publish', 'sandbox_packages'):
            option = []
            for line in trigger.split(f'      {name}:\n', 1)[1].splitlines():
                if line and not line.startswith('        '):
                    break
                option.append(line)
            self.assertIn('        type: boolean', option)
            self.assertIn('        default: false', option)
        self.assertIn('\npermissions:\n  contents: read\n', text)

        jobs = {}
        current = None
        for line in text.split('\njobs:\n', 1)[1].splitlines():
            if line.startswith('  ') and not line.startswith('   ') and line.endswith(':'):
                current = line.strip()[:-1]
                jobs[current] = []
            elif current is not None:
                jobs[current].append(line)
        self.assertIn('    if: ${{ inputs.publish }}', jobs['publish'])
        self.assertIn('    permissions:\n      contents: write', '\n'.join(jobs['publish']))
        self.assertIn('    if: ${{ inputs.sandbox_packages }}', jobs['sandbox-packages'])
        for name, lines in jobs.items():
            if name != 'publish':
                self.assertNotIn('contents: write', '\n'.join(lines))
        self.assertIn('    if: ${{ false }}', jobs['msix-bundle'])
        windows = '\n'.join(jobs['windows'])
        self.assertIn('      - name: MSIX package (Microsoft Store)\n        if: ${{ false }}', windows)
        self.assertIn('      - uses: actions/upload-artifact@v7\n        if: ${{ false }}\n        with:\n          name: msix-', windows)

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
