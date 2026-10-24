# -*- coding: utf-8 -*-
"""Real Lettuce adapter runnable unchanged by Python 2.7 and Python 3.

The installed provider determines interpreter compatibility. No provider code
is rewritten, and the Python-3-only common reporter is never imported here.
"""
from __future__ import print_function
import json
import os
import sys
import time
import traceback

REAL_STDOUT = sys.stdout
try:
    text_type = unicode
except NameError:
    text_type = str


def text(value):
    if isinstance(value, text_type):
        return value
    if isinstance(value, bytes):
        return value.decode('utf-8', 'replace')
    return text_type(value)


def emit(**event):
    # ASCII JSON is transport-safe even with a Python-2 byte stdout stream.
    REAL_STDOUT.write('##rustpy-test## ' + json.dumps(event, ensure_ascii=True) + '\n')
    REAL_STDOUT.flush()


class Capture(object):
    encoding = 'utf-8'

    def __init__(self):
        self.parts = []

    def write(self, value):
        self.parts.append(text(value))

    def flush(self):
        pass

    def isatty(self):
        return False

    def value(self):
        return u''.join(self.parts)


def arguments(values):
    root, cwd, native = None, None, []
    index = 0
    while index < len(values):
        value = values[index]
        if value == '--':
            native = values[index + 1:]
            break
        if value in ('--framework', '--root', '--cwd'):
            if index + 1 >= len(values):
                raise ValueError('Missing value for ' + value)
            if value == '--framework' and values[index + 1] != 'lettuce':
                raise ValueError('Lettuce bootstrap received another framework')
            if value == '--root':
                root = os.path.abspath(values[index + 1])
            if value == '--cwd':
                cwd = os.path.abspath(values[index + 1])
            index += 2
        else:
            raise ValueError('Unknown adapter argument: ' + value)
    if root is None:
        raise ValueError('Lettuce requires --root')
    return root, cwd or root, native


def plan(values):
    paths, tags, scenarios, failfast = [], None, None, False
    index = 0
    while index < len(values):
        value = values[index]
        if value in ('--tag', '--tags', '-t', '--scenarios', '-s'):
            if index + 1 >= len(values):
                raise ValueError('Missing Lettuce option value: ' + value)
            if value in ('--tag', '--tags', '-t'):
                tags = values[index + 1].split(',')
            else:
                scenarios = values[index + 1]
            index += 2
            continue
        if value == '--failfast':
            failfast = True
        elif value.startswith('-'):
            raise ValueError('Unsupported Lettuce option: ' + value)
        else:
            paths.append(value)
        index += 1
    return paths, tags, scenarios, failfast


def main():
    started = time.time()
    counts = dict(passed=0, failed=0, skipped=0, errors=0)
    originals = (sys.stdout, sys.stderr)
    hook_registry = None
    try:
        root, cwd, native = arguments(sys.argv[1:])
        os.chdir(cwd)
        sys.path.insert(0, root)
        sys.path.insert(0, cwd)
        try:
            import lettuce
        except ImportError:
            raise RuntimeError('lettuce is not installed for ' + sys.executable)
        paths, tags, scenarios, failfast = plan(native)
        active = {}
        hook_errors = []

        class ObservedHook(object):
            def __init__(self, callback):
                self.callback = callback

            def __getattr__(self, name):
                # Lettuce's registration deduplication compares func_code.
                # Delegate it and other callable metadata to the real hook.
                return getattr(self.callback, name)

            def __call__(self, *args, **kwargs):
                try:
                    return self.callback(*args, **kwargs)
                except BaseException:
                    # Runner.run masks escaping callback errors with the same
                    # LettuceRunnerError it uses for an expected failfast stop.
                    # Retain provenance before that native boundary loses it.
                    hook_errors.append(traceback.format_exc())
                    raise
                finally:
                    observe_loaded_hooks()

        @lettuce.before.all
        def observe_loaded_hooks():
            # This public lifecycle hook runs after terrain/step loading. Keep
            # registered callback order and arguments; never change provider
            # source or intercept the debugger's tracing facility.
            for situations in hook_registry.values():
                for callbacks in situations.values():
                    callbacks[:] = [callback if callback is observe_loaded_hooks
                                    or isinstance(callback, ObservedHook)
                                    else ObservedHook(callback)
                                    for callback in callbacks]

        from lettuce.registry import CALLBACK_REGISTRY
        hook_registry = CALLBACK_REGISTRY

        def identity(scenario):
            feature = getattr(scenario, 'feature', None)
            description = getattr(feature, 'described_at', None)
            filename = getattr(description, 'file', None) or getattr(scenario, 'with_file', None) or 'features'
            return '::'.join((text(os.path.relpath(filename, root)).replace('\\', '/'),
                              text(getattr(feature, 'name', 'Feature')), text(scenario.name)))

        @lettuce.before.each_scenario
        def before(scenario):
            key = identity(scenario)
            out, err = Capture(), Capture()
            active[id(scenario)] = (key, time.time(), out, err, [])
            emit(event='collected', id=key, name=text(scenario.name), line=getattr(getattr(scenario, 'described_at', None), 'line', None))
            emit(event='start', id=key)
            sys.stdout, sys.stderr = out, err

        @lettuce.after.each_step
        def after_step(step):
            owner = getattr(step, 'scenario', None)
            current = active.get(id(owner))
            if current is None and len(active) == 1:
                current = list(active.values())[0]
            if current is None:
                return
            why = getattr(step, 'why', None)
            if getattr(step, 'failed', False):
                exception = getattr(why, 'exception', None)
                detail = getattr(why, 'traceback', None)
                current[4].append(text(detail or exception or getattr(step, 'sentence', 'Failed step')))
            elif not getattr(step, 'has_definition', False):
                current[4].append('Undefined step: ' + text(getattr(step, 'sentence', '')))

        @lettuce.after.each_scenario
        def after(scenario):
            key, begin, out, err, details = active.pop(id(scenario))
            sys.stdout, sys.stderr = originals
            outcome = 'failed' if details else 'passed'
            counts[outcome] += 1
            detail = u'\n'.join(details)
            emit(event='result', id=key, outcome=outcome, duration=max(0.0, time.time() - begin),
                 message=detail.rstrip().split('\n')[-1] if detail else '', traceback=detail,
                 stdout=out.value(), stderr=err.value())

        for target in paths or [os.path.join(root, 'features')]:
            path, separator, selected = target.partition('::')
            path = path if os.path.isabs(path) else os.path.join(root, path)
            chosen = scenarios
            if separator:
                feature = lettuce.Feature.from_file(path)
                name = selected.rsplit('::', 1)[-1]
                matches = [str(index + 1) for index, scenario in enumerate(feature.scenarios) if scenario.name == name]
                if not matches:
                    raise ValueError('Lettuce scenario is unavailable: ' + selected)
                chosen = ','.join(matches)
            failures_before = counts['failed']
            try:
                lettuce.Runner(path, scenarios=chosen, tags=tags, failfast=failfast, verbosity=0).run()
            except lettuce.LettuceRunnerError as error:
                # Lettuce 0.2.23 rethrows its failfast stop as this sentinel
                # after after_each_scenario has already reported the failure.
                # Other provider/loading/hook errors remain infrastructure errors.
                if hook_errors:
                    raise RuntimeError('Lettuce callback failed:\n' + '\n'.join(hook_errors))
                if not (failfast and text(error) == 'Test failed.'
                        and counts['failed'] > failures_before and not active):
                    raise
            if hook_errors:
                raise RuntimeError('Lettuce callback failed:\n' + '\n'.join(hook_errors))
            if failfast and counts['failed']:
                break
        if not sum(counts.values()):
            raise RuntimeError('Lettuce did not execute any scenarios; check source loading, tags and selection')
        code = 1 if counts['failed'] or counts['errors'] else 0
    except BaseException:
        sys.stdout, sys.stderr = originals
        counts['errors'] += 1
        emit(event='error', message=traceback.format_exc())
        code = 2
    finally:
        sys.stdout, sys.stderr = originals
        if hook_registry is not None:
            for situations in hook_registry.values():
                for callbacks in situations.values():
                    callbacks[:] = [callback.callback if isinstance(callback, ObservedHook)
                                    else callback for callback in callbacks]
    emit(event='done', duration=max(0.0, time.time() - started), exit_code=code, **counts)
    return code


if __name__ == '__main__':
    sys.exit(main())
