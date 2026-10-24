"""Failure-site adapters for native test frameworks; enabled explicitly by IDE."""
import functools
import json
import os
import sys

_address = None

def debug_failure(exc_info):
    global _address
    if os.environ.get('RUSTPY_DEBUG_FAILED_TESTS') != '1' or not exc_info or exc_info[2] is None:
        return
    try:
        import debugpy
        if not debugpy.is_client_connected():
            if _address is None:
                _address = debugpy.listen(('127.0.0.1', 0))
            callback = os.environ.get('RUSTPY_DEBUG_TEST_CALLBACK')
            if callback:
                import socket
                host, port = callback.rsplit(':', 1)
                with socket.create_connection((host, int(port)), timeout=3) as channel:
                    channel.sendall((json.dumps(dict(token=os.environ.get('RUSTPY_DEBUG_TEST_TOKEN'), host=_address[0], port=_address[1], pid=os.getpid())) + '\n').encode('utf-8'))
                    acknowledgement = b''
                    while len(acknowledgement) < 3:
                        chunk = channel.recv(3 - len(acknowledgement))
                        if not chunk:
                            raise RuntimeError('Debugger callback closed')
                        acknowledgement += chunk
                    if acknowledgement != b'OK\n':
                        raise RuntimeError('Debugger callback rejected')
            else:
                sys.__stdout__.write('##rustpy-test## ' + json.dumps(dict(event='debugger', host=_address[0], port=_address[1])) + '\n')
                sys.__stdout__.flush()
            debugpy.wait_for_client()
        import threading
        from _pydevd_bundle.pydevd_constants import get_global_debugger, CMD_ADD_EXCEPTION_BREAK
        debugger = get_global_debugger()
        thread = threading.current_thread()
        tb = exc_info[2]
        while tb.tb_next is not None:
            tb = tb.tb_next
        debugger.set_suspend(thread, CMD_ADD_EXCEPTION_BREAK)
        debugger.do_wait_suspend(thread, tb.tb_frame, 'exception', exc_info)
    except Exception as error:
        sys.__stderr__.write('Cannot enter failed-test debugger: ' + str(error) + '\n')


def _wrap_exception(owner, name):
    original = getattr(owner, name)
    if getattr(original, '_rustpy_debug_failure', False):
        return
    @functools.wraps(original)
    def run(*args, **kwargs):
        try:
            return original(*args, **kwargs)
        except Exception:
            import unittest
            if not isinstance(sys.exc_info()[1], unittest.SkipTest):
                debug_failure(sys.exc_info())
            raise
    run._rustpy_debug_failure = True
    setattr(owner, name, run)


def _wrap_result(owner, name):
    original = getattr(owner, name)
    if getattr(original, '_rustpy_debug_failure', False):
        return
    @functools.wraps(original)
    def result(self, test, *args, **kwargs):
        error = (args[-1] if name == 'addSubTest' else args[0]) if args else None
        if error is not None:
            if isinstance(error, tuple):
                info = error
            else:
                info = (error.type, error.value, error.getTracebackObject())
            debug_failure(info)
        return original(self, test, *args, **kwargs)
    result._rustpy_debug_failure = True
    setattr(owner, name, result)


def _doctest():
    global _doctest_installed
    if globals().get('_doctest_installed', False):
        return
    _doctest_installed = True
    import doctest
    import types
    previous = sys.getprofile()
    last = [None]
    def profile(frame, event, value):
        if callable(previous):
            previous(frame, event, value)
        if event == 'return' and frame.f_code.co_filename.startswith('<doctest '):
            last[0] = frame
    sys.setprofile(profile)
    failure = doctest.DocTestRunner.report_failure
    unexpected = doctest.DocTestRunner.report_unexpected_exception
    def mismatch(self, out, test, example, got):
        frame, last[0] = last[0], None
        if frame is not None:
            error = AssertionError('Doctest output differs from expected output')
            debug_failure((type(error), error, types.TracebackType(None, frame, frame.f_lasti, frame.f_lineno)))
        return failure(self, out, test, example, got)
    def exception(self, out, test, example, info):
        debug_failure(info)
        return unexpected(self, out, test, example, info)
    doctest.DocTestRunner.report_failure = mismatch
    doctest.DocTestRunner.report_unexpected_exception = exception


def install(framework):
    if os.environ.get('RUSTPY_DEBUG_FAILED_TESTS') != '1':
        return
    if framework == 'behave':
        from behave.matchers import Match, MatchWithError
        _wrap_exception(Match, 'run')
        _wrap_exception(MatchWithError, 'run')
    elif framework == 'trial':
        # Keep Trial's --reactor selection ahead of reporter imports.
        module = sys.modules.get('twisted.trial.reporter')
        if module is None or not hasattr(module, 'TestResult'):
            install_import_hooks()
            return
        _wrap_result(module.TestResult, 'addFailure')
        _wrap_result(module.TestResult, 'addError')
    elif framework == 'lettuce':
        from lettuce.core import Step
        _wrap_exception(Step, 'run')
    elif framework == 'doctest':
        _doctest()
    elif framework == 'tox':
        # Tox keeps its own environment commands and interpreter selection.
        # Only debugger callback/plugin discovery variables cross its boundary.
        directory = os.path.dirname(__file__)
        os.environ['PYTHONPATH'] = directory + os.pathsep + os.environ.get('PYTHONPATH', '')
        os.environ['RUSTPY_DEBUG_TEST_AUTOINSTALL'] = '1'
        plugins = os.environ.get('PYTEST_PLUGINS', '').split(',')
        if 'rustpy_debug_failure_plugin' not in plugins:
            plugins.append('rustpy_debug_failure_plugin')
        os.environ['PYTEST_PLUGINS'] = ','.join(filter(None, plugins))
        names = ['RUSTPY_DEBUG_FAILED_TESTS', 'RUSTPY_DEBUG_TEST_CALLBACK', 'RUSTPY_DEBUG_TEST_TOKEN', 'RUSTPY_DEBUG_TEST_AUTOINSTALL', 'PYTHONPATH', 'PYTEST_PLUGINS']
        os.environ['TOX_TESTENV_PASSENV'] = os.environ.get('TOX_TESTENV_PASSENV', '') + ' ' + ' '.join(names)
        override = 'testenv.pass_env+=' + ','.join(names)
        os.environ['TOX_OVERRIDE'] = ';'.join(filter(None, [os.environ.get('TOX_OVERRIDE', ''), override]))
        import subprocess
        original = subprocess.Popen
        if not getattr(original, '_rustpy_debug_tox', False):
            @functools.wraps(original)
            def tox_process(*args, **kwargs):
                command = args[0] if args else kwargs.get('args')
                if isinstance(command, (list, tuple)) and '-m' in command:
                    module = command.index('-m')
                    if module + 1 < len(command) and command[module + 1] == 'tox':
                        selected = []
                        for index, argument in enumerate(command[:-1]):
                            if argument in ('-e', '--env', '--environment'):
                                selected.extend(command[index + 1].split(','))
                        environment = dict(os.environ if kwargs.get('env') is None else kwargs['env'])
                        overrides = [environment.get('TOX_OVERRIDE', '')]
                        for name in selected:
                            if name and all(character.isalnum() or character in '_-.' for character in name):
                                overrides.append('testenv:' + name + '.pass_env+=' + ','.join(names))
                        environment['TOX_OVERRIDE'] = ';'.join(filter(None, overrides))
                        kwargs['env'] = environment
                return original(*args, **kwargs)
            tox_process._rustpy_debug_tox = True
            subprocess.Popen = tox_process

    elif framework == 'child':
        import unittest
        _wrap_result(unittest.TestResult, 'addFailure')
        _wrap_result(unittest.TestResult, 'addError')
        _wrap_result(unittest.TestResult, 'addSubTest')
        _doctest()


def install_import_hooks():
    """Native commands import their selected framework lazily."""
    global _import_hooks_installed
    if globals().get('_import_hooks_installed', False):
        return
    _import_hooks_installed = True
    import builtins
    original = builtins.__import__
    busy = [False]
    @functools.wraps(original)
    def importing(name, *args, **kwargs):
        module = original(name, *args, **kwargs)
        if not busy[0]:
            busy[0] = True
            try:
                for prefix, framework, owner, member in [('behave.matchers', 'behave', 'behave.matchers', 'Match'), ('twisted.trial.reporter', 'trial', 'twisted.trial.reporter', 'TestResult'), ('lettuce.core', 'lettuce', 'lettuce.core', 'Step')]:
                    if name.startswith(prefix) and hasattr(sys.modules.get(owner), member):
                        install(framework)
            finally:
                busy[0] = False
        return module
    builtins.__import__ = importing
