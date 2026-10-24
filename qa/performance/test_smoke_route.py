"""UNEXECUTED: planning/identity fixtures, never launch GUI or child processes."""
import hashlib
from pathlib import Path
import unittest
from unittest.mock import MagicMock
import json
from driver import (ROADMAP_TARGET_PROFILE, SMOKE, combined_result, descendants, input_commands, measured_binary, million_result,
                    native_contract, process_tree_rss, smoke_binary, smoke_command, smoke_environment, test_executable, worker_plan)


class SmokeRouteContracts(unittest.TestCase):
    def test_only_unchanged_wrapper_is_the_native_entrypoint(self):
        command = smoke_command(Path('/fixture'),Path('/fixture/a.py'),Path('/evidence/input'),Path('/evidence'))
        self.assertTrue(command[0].endswith('/scripts/smoke.sh'))
        self.assertNotIn('rustpy',command[1:])
        self.assertIn('--input-script',command)

    def test_inherited_desktop_and_broad_trust_are_removed(self):
        before = dict(DISPLAY=':0',WAYLAND_DISPLAY='real-desktop',RUSTPY_TRUST_ALL='1',RUSTPY_CONFIG_DIR='/isolated/config')
        after = smoke_environment(before,70,'release',300,295)
        self.assertNotIn('DISPLAY',after)
        self.assertNotIn('WAYLAND_DISPLAY',after)
        self.assertNotIn('RUSTPY_TRUST_ALL',after)
        self.assertEqual(after['RUSTPY_SMOKE_UNTRUSTED'],'1')
        self.assertEqual(after['RUSTPY_SMOKE_DELAY'],'70')
        self.assertEqual(before['DISPLAY'],':0')
        self.assertEqual(after['RUSTPY_SMOKE_PROFILE'],'release')
        self.assertEqual(after['RUSTPY_SMOKE_TIMEOUT'],'300')
        self.assertEqual(after['RUSTPY_SMOKE_AWAIT_SCRIPT'],'1')
        self.assertEqual(after['RUSTPY_SMOKE_DEADLINE'],'295')

    def test_native_budget_is_configurable_and_capture_waits_within_it(self):
        self.assertEqual(native_contract(90,'software'),(70,85))
        self.assertEqual(native_contract(300,'software'),(90,295))
        for seconds,renderer in ((901,'software'),(59,'software'),(90,'gpu')):
            with self.assertRaises(ValueError): native_contract(seconds,renderer)

    def test_smoke_wrapper_selects_profile_binary_and_timeout(self):
        text = SMOKE.read_text()
        self.assertIn('RUSTPY_SMOKE_PROFILE:-debug',text)
        self.assertIn('RUSTPY_SMOKE_TIMEOUT:-120',text)
        self.assertIn('xvfb-run',text)
        self.assertTrue(str(smoke_binary('release')).endswith('target/release/rustpy'))

    def test_worker_count_and_core_cap_are_explicit(self):
        self.assertEqual(worker_plan(10),(10,9))
        self.assertEqual(worker_plan(1),(1,1))
        self.assertEqual(worker_plan(10,10),(10,10))
        for cores,workers in ((0,None),(4,0)):
            with self.assertRaises(ValueError): worker_plan(cores,workers)

    def test_script_steps_are_marked_and_waits_scale(self):
        commands,marks = input_commands('native10k',2.0)
        self.assertEqual(marks,['start','typing','power-save-on','power-save-off','invalidate-caches','invalidate-caches-apply','save','end'])
        # The action opens the Invalidate Caches dialog; a second Enter (its
        # default button) applies it before the script moves on.
        opened = commands.index('typefast Invalidate Caches')
        applied = commands.index('mark invalidate-caches-apply')
        self.assertEqual(commands[opened+2],'key Enter')
        self.assertLess(opened,applied)
        self.assertEqual(commands[applied+1],'key Enter')
        self.assertTrue(commands[applied+2].startswith('wait '))
        self.assertIn('wait 6',commands)
        self.assertEqual(commands[-1],'mark end')
        _,marks = input_commands('largefile')
        self.assertEqual(marks,['start','navigate','save','end'])

    def test_million_runs_the_built_test_executable_not_cargo(self):
        messages = [json.dumps(dict(reason='compiler-artifact',target=dict(name='rustpy_analysis',kind=['lib']),executable=None)),
                    'not json',
                    json.dumps(dict(reason='compiler-artifact',target=dict(name='million_line_project',kind=['test']),executable='/t/release/deps/million_line_project-abc')),
                    json.dumps(dict(reason='build-finished',success=True))]
        self.assertEqual(str(test_executable(messages)),'/t/release/deps/million_line_project-abc')
        with self.assertRaises(ValueError): test_executable(messages[:2])

    def test_million_result_requires_complete_output_within_limits(self):
        raw = '1000064 lines (900032 nonblank Python lines) / 128 files: 12s\ntest result: ok. 1 passed; 0 failed'
        metadata = dict(timed_out=False,cpus=list(range(10)),workers=9,limit_seconds=300)
        self.assertEqual(million_result(raw,0,metadata)['status'],'pass')
        self.assertEqual(million_result(raw,0,dict(metadata,timed_out=True))['status'],'fail')
        self.assertEqual(million_result(raw,0,dict(metadata,memory_limit_exceeded=True))['status'],'fail')
        self.assertEqual(million_result('1 passed; 0 failed',0,metadata)['status'],'fail')

    def test_both_profiles_are_reported_and_release_carries_the_roadmap_target(self):
        self.assertEqual(ROADMAP_TARGET_PROFILE,'release')
        result = combined_result(dict(debug=dict(status='fail'),release=dict(status='pass')))
        self.assertEqual(result['status'],'pass')
        self.assertFalse(result['profiles']['debug']['applies_to_roadmap_target'])
        self.assertTrue(result['profiles']['release']['applies_to_roadmap_target'])
        self.assertEqual(combined_result(dict(debug=dict(status='pass')))['status'],'fail')

    def test_different_requested_binary_cannot_be_measured_under_smoke_hash(self):
        requested = MagicMock(spec=Path)
        actual = MagicMock(spec=Path)
        requested.is_file.return_value = actual.is_file.return_value = True
        requested.read_bytes.return_value = b'alternate-release'
        actual.read_bytes.return_value = b'actual-smoke-debug'
        with self.assertRaises(ValueError): measured_binary(requested,actual)
        requested.read_bytes.return_value = b'actual-smoke-debug'
        _,digest=measured_binary(requested,actual)
        self.assertEqual(digest,hashlib.sha256(b'actual-smoke-debug').hexdigest())

    def test_app_tree_excludes_wrapper_xvfb_but_safety_tree_includes_them(self):
        # xvfb-run -> Xvfb sibling and timeout -> actual RustPy -> owned helper.
        parents={10:1,11:10,12:10,13:12,14:13,90:1}
        resident={10:10,11:500,12:10,13:100,14:30,90:9999}
        self.assertEqual(descendants(13,parents),{13,14})
        self.assertEqual(process_tree_rss(13,parents,resident),130)
        self.assertEqual(process_tree_rss(10,parents,resident),650)
