"""Stdlib doctest adapter for the existing structured test-results protocol."""

def run_doctest(root, args, emit, import_file):
    import contextlib
    import doctest
    import io
    import os
    import time
    import traceback
    options = 0
    targets = []
    failfast = False
    it = iter(args)
    for arg in it:
        if arg in ('-v', '--verbose'):
            continue
        if arg in ('-f', '--fail-fast'):
            failfast = True
            options |= doctest.FAIL_FAST
        elif arg in ('-o', '--option'):
            option = next(it, '')
            if option not in doctest.OPTIONFLAGS_BY_NAME:
                raise ValueError('Unknown doctest option: ' + option)
            options |= doctest.OPTIONFLAGS_BY_NAME[option]
        elif arg.startswith('-'):
            raise ValueError('Unsupported doctest option: ' + arg)
        else:
            targets.append(arg)
    if not targets:
        raise ValueError('Select at least one Python file or doctest node')
    started = time.perf_counter()
    counts = dict(passed=0, failed=0, skipped=0, errors=0)
    seen = set()
    for target in targets:
        path, _, selection = target.partition('::')
        path = os.path.abspath(os.path.join(root, path))
        try:
            if not path.endswith('.py') or not os.path.isfile(path):
                raise ValueError('Doctest target must be a Python source file')
            captured = io.StringIO()
            with contextlib.redirect_stdout(captured), contextlib.redirect_stderr(captured):
                module = import_file(path)
                tests = doctest.DocTestFinder(exclude_empty=True).find(module)
            for test in tests:
                prefix = module.__name__ + '.'
                qualname = test.name[len(prefix):] if test.name.startswith(prefix) else test.name
                if test.name == module.__name__:
                    qualname = '<module>'
                if selection and qualname != selection.replace('::', '.'):
                    continue
                node = os.path.relpath(path, root).replace(os.sep, '/') + '::' + qualname
                if node in seen:
                    continue
                seen.add(node)
                emit(event='collected', id=node, name=qualname, file=os.path.relpath(path, root), line=(test.lineno or 0) + 1)
                emit(event='start', id=node)
                output = io.StringIO()
                class Reporter(doctest.DocTestRunner):
                    expected = None
                    actual = None
                    def report_failure(self, out, test, example, got):
                        self.expected = example.want
                        self.actual = got
                        super().report_failure(out, test, example, got)
                runner = Reporter(verbose=False, optionflags=options)
                begin = time.perf_counter()
                # stdlib owns example evaluation, whitespace/ellipsis flags and shared globals.
                result = runner.run(test, out=output.write)
                failed = bool(result.failed)
                skipped = not result.attempted and all(example.options.get(doctest.SKIP, False) for example in test.examples)
                outcome = 'failed' if failed else ('skipped' if skipped else 'passed')
                counts[outcome] += 1
                emit(event='result', id=node, outcome=outcome, duration=time.perf_counter()-begin,
                     message=output.getvalue()[:200000], traceback=output.getvalue()[:200000], stdout=captured.getvalue()[:200000],
                     expected=runner.expected, actual=runner.actual)
                if failed and failfast:
                    break
            if selection and not any(key == os.path.relpath(path, root).replace(os.sep, '/')+'::'+selection.replace('::','.') for key in seen):
                raise ValueError('Selected doctest was not found: ' + selection)
        except Exception:
            counts['errors'] += 1
            message = traceback.format_exc()
            emit(event='collected', id=target, name=target, file=os.path.relpath(path, root), line=1)
            emit(event='start', id=target)
            emit(event='result', id=target, outcome='error', message=message, traceback=message)
            emit(event='error', message=message)
        if failfast and (counts['failed'] or counts['errors']):
            break
    code = int(bool(counts['failed'] or counts['errors']))
    emit(event='done', duration=time.perf_counter()-started, exit_code=code, **counts)
    return code
