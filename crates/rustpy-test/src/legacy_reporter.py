"""Native legacy framework adapters producing the common live JSON protocol."""
import json
import os
import subprocess
import sys
import tempfile
import time
import traceback


def run(framework, root, args, api):
    emit = api['emit']
    Reporter = api['UnittestReporter']
    node_id = api['unittest_node_id']
    reporter = Reporter(root)
    started = time.perf_counter()

    def begin(test):
        emit(event='collected', id=node_id(test, root), name=str(test), line=api['test_line'](test))
        reporter.startTest(test)

    # Framework-specific selectors keep all plugin options verbatim; only the
    # IDE's file::Class::method notation is translated to each native spelling.
    def selectors(values, nose=False):
        converted = []
        for value in values:
            file, separator, member = value.partition('::')
            if separator:
                value = (file + ':' + member.replace('::', '.')) if nose else api['to_unittest_target'](value, root)
                if not nose:
                    kind, path, name = value
                    if kind == 'file':
                        module = api['import_test_file'](path)
                        value = module.__name__ + ('.' + name if name else '')
                    else:
                        value = path
            converted.append(value)
        return converted

    try:
        if framework == 'nose':
            import nose
            from nose.plugins.base import Plugin

            class JsonReporter(Plugin):
                name = 'rustpy'
                enabled = True
                def configure(self, options, conf):
                    self.enabled = True
                def startTest(self, test): begin(test)
                def stopTest(self, test): reporter.stopTest(test)
                def addError(self, test, err): reporter.addError(test, err)
                def addFailure(self, test, err): reporter.addFailure(test, err)
                def addSkip(self, test, reason='skipped'): reporter.addSkip(test, reason)
                def addSuccess(self, test): pass

            ok = nose.run(argv=['nosetests'] + selectors(args, True), addplugins=[JsonReporter()])
            code = 0 if ok else 1
        elif framework == 'nose2':
            from nose2.main import PluggableTestProgram

            class Hooks:
                def startTest(self, event): begin(event.test)
                def stopTest(self, event):
                    if reporter._current is not None: reporter.stopTest(event.test)
                def testOutcome(self, event):
                    synthetic = reporter._current is None
                    if synthetic: begin(event.test)
                    if event.outcome == 'subtest':
                        parent = getattr(event.test, 'test_case', event.test)
                        reporter.addSubTest(parent, event.test, event.exc_info)
                    elif event.outcome == 'failed' and getattr(event, 'expected', False): reporter.addExpectedFailure(event.test, event.exc_info)
                    elif event.outcome == 'failed': reporter.addFailure(event.test, event.exc_info)
                    elif event.outcome == 'error': reporter.addError(event.test, event.exc_info)
                    elif event.outcome == 'skipped': reporter.addSkip(event.test, getattr(event, 'reason', 'skipped'))
                    elif event.outcome == 'passed' and not getattr(event, 'expected', True): reporter.addUnexpectedSuccess(event.test)
                    if synthetic: reporter.stopTest(event.test)

            hooks = Hooks()
            program = PluggableTestProgram(module=None, argv=['nose2'] + selectors(args), exit=False,
                extraHooks=[(name, hooks) for name in ('startTest', 'stopTest', 'testOutcome')])
            code = 0 if program.result.wasSuccessful() else 1
        elif framework == 'lettuce':
            return run_lettuce(root, args, api)
        elif framework == 'tox':
            return run_tox(root, args, api)
        else:
            raise ValueError('Unsupported framework: ' + framework)
    except BaseException:
        emit(event='error', message=traceback.format_exc())
        code = 2
    emit(event='done', duration=time.perf_counter()-started, exit_code=code, **reporter.counts)
    return code


def expand_environments(value):
    pieces, start, depth = [], 0, 0
    for index, character in enumerate(value):
        if character == '{': depth += 1
        elif character == '}': depth = max(0, depth-1)
        elif depth == 0 and (character == ',' or character.isspace()):
            if index > start: pieces.append(value[start:index])
            start = index+1
    if start < len(value): pieces.append(value[start:])
    result = []
    for piece in pieces:
        if '{' in piece and '}' in piece:
            before, tail = piece.split('{',1)
            choices, after = tail.split('}',1)
            for choice in choices.split(','):
                result.extend(expand_environments(before+choice+after))
        else: result.append(piece)
        if len(result) >= 256: return result[:256]
    return result


def run_tox(root, args, api):
    """Run tox's real commands; each environment has a distinct result identity.

    Environment tabs can call this adapter once per selected environment. The
    JSON result records contain real tox command status/output, including setup
    failures; no guessed success is inferred from console text.
    """
    emit = api['emit']
    started = time.perf_counter()
    environments, remaining = [], []
    index = 0
    while index < len(args):
        value = args[index]
        if value in ('-e', '--env', '--environment') and index+1 < len(args):
            environments.extend(expand_environments(args[index+1])); index += 2; continue
        if value.startswith(('-e=', '--env=', '--environment=')):
            environments.extend(expand_environments(value.split('=',1)[1]))
        else:
            remaining.append(value)
        index += 1
    if not environments:
        listing = subprocess.run([sys.executable, '-m', 'tox', '-l'], cwd=root, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        if listing.returncode:
            emit(event='error', message=listing.stderr or listing.stdout or 'tox environment discovery failed')
            return listing.returncode
        environments = [line.strip() for line in listing.stdout.splitlines() if line.strip()]
    counts = dict(passed=0, failed=0, skipped=0, errors=0)
    for environment in dict.fromkeys(environments):
        if not environment or environment.startswith('-'):
            raise ValueError('Invalid tox environment')
        identity = 'tox::' + environment
        emit(event='collected', id=identity, name=environment)
        emit(event='metadata', id=identity, environment=environment)
        emit(event='start', id=identity)
        t0 = time.perf_counter()
        with tempfile.TemporaryDirectory(prefix='tox-report-') as directory:
            report = os.path.join(directory, 'result.json')
            command = [sys.executable, '-m', 'tox', '-e', environment, '--result-json', report] + remaining
            process = subprocess.Popen(command, cwd=root, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, errors='replace')
            output = []
            for line in process.stdout:
                sys.stdout.write(line); sys.stdout.flush()
                output.append(line)
            code = process.wait()
            details = ''
            if os.path.isfile(report):
                with open(report, encoding='utf-8') as stream:
                    details = json.dumps(json.load(stream).get('testenvs', {}).get(environment, {}), ensure_ascii=False, indent=2)
            outcome = 'passed' if code == 0 else 'failed'
            counts[outcome] += 1
            emit(event='result', id=identity, outcome=outcome, duration=time.perf_counter()-t0,
                 message='' if code == 0 else 'tox environment exited with code %d' % code,
                 traceback=details if code else '', stdout=''.join(output), stderr='')
    code = 1 if counts['failed'] or counts['errors'] else 0
    emit(event='done', duration=time.perf_counter()-started, exit_code=code, **counts)
    return code


def run_lettuce(root, args, api):
    import lettuce
    emit = api['emit']
    counts = dict(passed=0, failed=0, skipped=0, errors=0)
    started = time.perf_counter()
    identities = {}

    def identity(scenario):
        feature = getattr(scenario, 'feature', None)
        filename = getattr(feature, 'filename', None) or getattr(feature, 'file', 'features')
        return '%s::%s::%s' % (api['rel'](filename, root), getattr(feature, 'name', 'Feature'), scenario.name)

    @lettuce.before.each_scenario
    def before(scenario):
        key = identity(scenario)
        identities[id(scenario)] = (key, time.perf_counter())
        emit(event='collected', id=key, name=scenario.name, line=getattr(scenario, 'line', None))
        emit(event='start', id=key)

    @lettuce.after.each_scenario
    def after(scenario):
        key, t0 = identities.pop(id(scenario), (identity(scenario), time.perf_counter()))
        steps = getattr(scenario, 'steps', [])
        failed = [step for step in steps if getattr(step, 'failed', False) or getattr(step, 'error', None)]
        outcome = 'failed' if failed else 'passed'
        counts[outcome] += 1
        details = '\n'.join(str(getattr(step, 'error', '') or getattr(step, 'exception', '') or getattr(step, 'sentence', '')) for step in failed)
        emit(event='result', id=key, outcome=outcome, duration=time.perf_counter()-t0, message=details.split('\n')[0], traceback=details, stdout='', stderr='')

    # Pass only documented Runner options. Unsupported switches are diagnosed,
    # never silently consumed or replaced by a different test framework.
    paths, tags, failfast, scenarios = [], None, False, None
    index = 0
    while index < len(args):
        value = args[index]
        if value in ('--tag', '--tags', '-t', '--scenarios', '-s') and index+1 < len(args):
            if value in ('--tag', '--tags', '-t'): tags = args[index+1].split(',')
            else: scenarios = args[index+1]
            index += 2; continue
        if value == '--failfast': failfast = True
        elif value.startswith('-'): raise ValueError('Unsupported Lettuce option: ' + value)
        else: paths.append(value)
        index += 1
    for target in paths or [os.path.join(root, 'features')]:
        path, separator, selected = target.partition('::')
        chosen = scenarios
        if separator:
            feature = lettuce.Feature.from_file(path)
            name = selected.rsplit('::', 1)[-1]
            matches = [str(index+1) for index, scenario in enumerate(feature.scenarios) if scenario.name == name]
            if not matches:
                raise ValueError('Lettuce scenario is unavailable: ' + selected)
            chosen = ','.join(matches)
        lettuce.Runner(path, scenarios=chosen, tags=tags, failfast=failfast, verbosity=0).run()
    code = 1 if counts['failed'] or counts['errors'] else 0
    emit(event='done', duration=time.perf_counter()-started, exit_code=code, **counts)
    return code
