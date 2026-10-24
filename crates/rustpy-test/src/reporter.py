"""RustPy test reporter.

Runs pytest or unittest and streams one JSON event per line to stdout, each
prefixed with ``##rustpy-test## `` so the IDE can separate them from normal
program output.  Usage (invoked by the IDE):

    python -u reporter.py --framework pytest|unittest --root DIR [--cwd DIR] -- [args...] [targets...]

``--root`` is the project root (node ids are relative to it), ``--cwd`` the
working directory of the tests (default: the root).

Events: collected, start, result, done, error (see the Rust side for fields).
A failed equality assertion adds ``expected`` / ``actual`` to its result.
"""
import fnmatch
import functools
import inspect
import io
import json
import os
import pprint
import re
import sys
import time
import traceback
import unittest

# The launched script and the importable pytest/xdist plugin must share one
# reporter module. Otherwise unittest wrappers capture __main__.CAPTURED while
# the plugin reads a newly imported module's unrelated empty dictionary.
if __name__ == "__main__":
    sys.modules["rustpy_test_reporter"] = sys.modules[__name__]

PREFIX = "##rustpy-test## "
REAL_STDOUT = sys.stdout
# Frames of this module are runner internals: unittest cuts failure tracebacks
# at them (the equality-capturing wrappers below would show up otherwise).
__unittest = True

MAX_COMPARED_CHARS = 200000


def emit(**event):
    REAL_STDOUT.write(PREFIX + json.dumps(event, ensure_ascii=False, default=str) + "\n")
    REAL_STDOUT.flush()


def debug_failure(exc_info):
    """Suspend with the retained exception traceback, including unwound frames."""
    if os.environ.get('RUSTPY_DEBUG_FAILED_TESTS') != '1' or not exc_info or exc_info[2] is None:
        return
    try:
        import rustpy_debug_failures
    except ImportError:
        pass
    else:
        return rustpy_debug_failures.debug_failure(exc_info)
    try:
        import debugpy
        if not debugpy.is_client_connected():
            address = getattr(debug_failure, 'address', None)
            if address is None:
                address = debugpy.listen(('127.0.0.1', 0))
                debug_failure.address = address
            emit(event='debugger', host=address[0], port=address[1])
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
        # Installation/adapter incompatibility must preserve the test result.
        emit(event='output', text='Cannot enter failed-test debugger: ' + str(error), stderr=True)


def compared_text(value):
    """A compared value as shown in the IDE's diff viewer: strings verbatim,
    anything else pretty-printed (sorted dict keys), capped in size."""
    if isinstance(value, str):
        text = value
    else:
        try:
            text = pprint.pformat(value, width=88)
        except Exception:
            try:
                text = repr(value)
            except Exception:
                text = "<unprintable %s>" % type(value).__name__
    if len(text) > MAX_COMPARED_CHARS:
        text = text[:MAX_COMPARED_CHARS] + "\n... (truncated)"
    return text


# The last failing unittest equality assertion: (exception, expected, actual).
CAPTURED = {"value": None}

EQUALITY_ASSERTIONS = (
    "assertEqual",
    "assertMultiLineEqual",
    "assertSequenceEqual",
    "assertListEqual",
    "assertTupleEqual",
    "assertSetEqual",
    "assertDictEqual",
    "assertCountEqual",
)


def compared_values(signature, args, kwargs):
    """The two values an equality assertion compared: its first two
    parameters after ``self`` (``first``/``second``, ``d1``/``d2``,
    ``list1``/``list2``…), passed by position or by keyword."""
    if signature is None:
        return (args[0], args[1]) if len(args) >= 2 else None
    names = list(signature.parameters)[1:3]
    bound = signature.bind(None, *args, **kwargs).arguments
    if len(names) == 2 and names[0] in bound and names[1] in bound:
        return bound[names[0]], bound[names[1]]
    return None


def install_equality_capture():
    """Wraps the TestCase equality assertions so that a failing one records
    its two values (``first`` is the expected value). The
    wrapper passes every call through unchanged, keyword arguments included."""

    def wrap(original):
        try:
            signature = inspect.signature(original)
        except (TypeError, ValueError):
            signature = None

        @functools.wraps(original)
        def wrapper(self, *args, **kwargs):
            __tracebackhide__ = True  # noqa: F841 - hides this frame in pytest
            try:
                return original(self, *args, **kwargs)
            except AssertionError as e:
                try:
                    values = compared_values(signature, args, kwargs)
                    if values is not None:
                        CAPTURED["value"] = (e, compared_text(values[0]), compared_text(values[1]))
                except Exception:
                    pass
                raise

        wrapper._rustpy_wrapped = True
        return wrapper

    for name in EQUALITY_ASSERTIONS:
        original = getattr(unittest.TestCase, name, None)
        if original is not None and not getattr(original, "_rustpy_wrapped", False):
            setattr(unittest.TestCase, name, wrap(original))


def captured_for(exc):
    """``(expected, actual)`` recorded for the exception ``exc``, if any."""
    value = CAPTURED["value"]
    if value is not None and exc is not None and value[0] is exc:
        return value[1], value[2]
    return None


# Packages whose frames lie between a failing `assert` statement and the
# pytest_assertrepr_compare hook (assertion rewriting, the plugin manager).
HOOK_CALLER_PACKAGES = ("_pytest", "pluggy")


def assert_location():
    """``(frame, line)`` of the ``assert`` statement pytest is explaining:
    the innermost caller outside pytest, pluggy and this module."""
    frame = sys._getframe(1)
    while frame is not None:
        module = frame.f_globals.get("__name__") or ""
        if frame.f_globals is not globals() and module.split(".")[0] not in HOOK_CALLER_PACKAGES:
            return frame, frame.f_lineno
        frame = frame.f_back
    return None


def raise_location(exc):
    """``(frame, line)`` where ``exc`` was raised (its innermost traceback entry)."""
    tb = getattr(exc, "__traceback__", None)
    if tb is None:
        return None
    while tb.tb_next is not None:
        tb = tb.tb_next
    return tb.tb_frame, tb.tb_lineno


def count_assert_explanations(reporter):
    """Tells ``reporter`` each time an ``assert`` was explained: pytest's
    rewritten asserts call ``_format_explanation`` once per failure (and per
    pass with the assertion-pass hook), after the comparison hooks and
    just before they raise. Without that helper nothing is counted."""
    try:
        from _pytest.assertion import rewrite
    except Exception:
        return
    original = getattr(rewrite, "_format_explanation", None)
    if original is None or getattr(original, "_rustpy_wrapped", False):
        return

    @functools.wraps(original)
    def _format_explanation(explanation):
        try:
            reporter.assert_explained(sys._getframe(1))
        except Exception:
            pass
        return original(explanation)

    _format_explanation._rustpy_wrapped = True
    rewrite._format_explanation = _format_explanation


def rel(path, root):
    try:
        p = os.path.abspath(path)
        r = os.path.abspath(root)
        if p == r or p.startswith(r + os.sep):
            return os.path.relpath(p, r).replace(os.sep, "/")
    except Exception:
        pass
    return str(path).replace(os.sep, "/")


def first_line(text):
    for line in (text or "").splitlines():
        if line.strip():
            return line.strip()
    return ""


# --------------------------------------------------------------------------
# pytest
# --------------------------------------------------------------------------


class PytestReporter:
    def __init__(self, root):
        self.root = root
        self.counts = {"passed": 0, "failed": 0, "skipped": 0, "errors": 0}
        self.started = time.perf_counter()
        self.reported = set()
        self.outcomes = {}
        self.metadata = {}
        self.worker = os.environ.get("PYTEST_XDIST_WORKER")
        # (assert location, explanations at that location before it,
        # expected, actual) of the failing `==` assertions explained so far in
        # the current test (values None when the operands do not differ), how
        # many asserts were explained at each location (see
        # count_assert_explanations), and the exception of the phase being
        # reported.
        self.comparisons = []
        self.explained = {}
        self.last_exc = None

    def assert_explained(self, frame):
        location = (frame, frame.f_lineno)
        self.explained[location] = self.explained.get(location, 0) + 1

    def pytest_assertrepr_compare(self, config, op, left, right):
        # Called while pytest explains a failed `assert`, just before it
        # raises; record the values of an `==` with the location of that
        # assert and keep pytest's own explanation (return None).
        if op != "==":
            return None
        try:
            differ = bool(left != right)
        except Exception:
            differ = True
        try:
            values = (compared_text(right), compared_text(left)) if differ else (None, None)
            location = assert_location()
            before = self.explained.get(location, 0) if location is not None else 0
            self.comparisons.append((location, before) + values)
        except Exception:
            pass
        return None

    def pytest_runtest_makereport(self, item, call):
        excinfo = getattr(call, "excinfo", None)
        self.last_exc = excinfo.value if excinfo is not None else None
        if excinfo is not None and getattr(call, 'when', '') == 'call' and not list(item.iter_markers('xfail')):
            import pytest
            if not isinstance(excinfo.value, (pytest.skip.Exception, pytest.xfail.Exception)):
                debug_failure((excinfo.type, excinfo.value, excinfo.tb))
        return None

    def pytest_runtest_logfinish(self, nodeid, location):
        # The recorded assert locations hold frames: release them with the test.
        self.comparisons = []
        self.explained = {}
        self.last_exc = None

    def _comparison(self):
        exc = self.last_exc
        found = captured_for(exc)
        if found is not None:
            return found
        if not isinstance(exc, AssertionError):
            return None
        # Only the explanation of the assert that raised `exc` describes it: a
        # comparison that failed earlier (and was caught, or belongs to another
        # phase) is another assertion.
        where = raise_location(exc)
        if where is None:
            return None
        explained = self.explained.get(where)
        for location, before, expected, actual in reversed(self.comparisons):
            if location is not None and location[0] is where[0] and location[1] == where[1]:
                # The same assert can fail again (in a loop, the first failure
                # caught) without reaching its `==`: that comparison then
                # belongs to the earlier failure.
                if explained is not None and before != explained - 1:
                    return None
                return (expected, actual) if expected is not None else None
        return None

    def pytest_itemcollected(self, item):
        if self.worker:
            return
        try:
            path, line, _ = item.location
        except Exception:
            path, line = None, None
        emit(
            event="collected",
            id=item.nodeid,
            name=item.name,
            file=rel(os.path.join(self.root, path), self.root) if path else None,
            line=(line + 1) if isinstance(line, int) else None,
        )

    def pytest_xdist_node_collection_finished(self, node, ids):
        for nodeid in ids:
            emit(event="collected", id=nodeid, name=nodeid.rsplit("::", 1)[-1], file=nodeid.split("::", 1)[0])

    def pytest_collectreport(self, report):
        if report.failed:
            self.counts["errors"] += 1
            text = report.longreprtext
            emit(
                event="result",
                id=report.nodeid or "<collection>",
                outcome="error",
                duration=0.0,
                message=first_line(text),
                traceback=text,
                stdout="",
                stderr="",
            )

    def pytest_runtest_logstart(self, nodeid, location):
        if self.worker:
            return
        self.comparisons = []
        self.explained = {}
        self.last_exc = None
        CAPTURED["value"] = None
        emit(event="start", id=nodeid)

    def _message(self, report):
        longrepr = getattr(report, "longrepr", None)
        if report.outcome == "skipped":
            if isinstance(longrepr, tuple) and len(longrepr) == 3:
                return str(longrepr[2])
            if hasattr(report, "wasxfail"):
                return "xfail: " + str(report.wasxfail)
        crash = getattr(longrepr, "reprcrash", None)
        if crash is not None and getattr(crash, "message", None):
            return crash.message
        return first_line(report.longreprtext)

    def pytest_runtest_logreport(self, report):
        if self.worker:
            return
        for name, value in getattr(report, "user_properties", []):
            if name == "rustpy_metadata":
                try:
                    incoming = json.loads(value)
                    previous = self.metadata.setdefault(report.nodeid, {})
                    for key, field in incoming.items():
                        if key in ("logs", "hypothesis"):
                            if field:
                                old = previous.get(key, "")
                                previous[key] = old + ("\n" if old else "") + field if field not in old else old
                        elif field is not None:
                            previous[key] = field
                    emit(event="metadata", **previous)
                except (ValueError, TypeError):
                    pass
        nodeid = report.nodeid
        when = report.when
        outcome = report.outcome
        if when == "call":
            pass
        elif when == "setup" and outcome != "passed":
            if outcome == "failed":
                outcome = "error"
        elif when == "teardown" and outcome == "failed":
            outcome = "error"
        else:
            return
        if nodeid in self.reported and when != "teardown":
            return
        self.reported.add(nodeid)
        prior = self.outcomes.get(nodeid)
        if prior is not None:
            prior_key = "errors" if prior == "error" else prior
            self.counts[prior_key] = max(0, self.counts.get(prior_key, 0) - 1)
        self.outcomes[nodeid] = outcome
        key = outcome if outcome != "error" else "errors"
        self.counts[key] = self.counts.get(key, 0) + 1
        message = self._message(report)
        if when == "teardown":
            message = "teardown failed: " + message
        comparison = self._comparison() if outcome in ("failed", "error") else None
        for property_name, property_value in getattr(report, "user_properties", []):
            if property_name == "rustpy_comparison":
                comparison = property_value
        emit(
            event="result",
            id=nodeid,
            outcome=outcome,
            duration=float(getattr(report, "duration", 0.0) or 0.0),
            message=message,
            traceback=report.longreprtext if outcome in ("failed", "error") else "",
            stdout=getattr(report, "capstdout", "") or "",
            stderr=getattr(report, "capstderr", "") or "",
            xfail=hasattr(report, "wasxfail"),
            expected=comparison[0] if comparison else None,
            actual=comparison[1] if comparison else None,
        )

    def pytest_sessionfinish(self, session, exitstatus):
        if self.worker:
            return
        try:
            code = int(exitstatus)
        except Exception:
            code = -1
        emit(event="done", duration=time.perf_counter() - self.started, exit_code=code, **self.counts)


PytestReporter.pytest_xdist_node_collection_finished.pytest_impl = {"optionalhook": True}


def run_pytest(root, args):
    try:
        import pytest
    except ImportError:
        emit(
            event="error",
            message="pytest is not installed for %s. Install it with: %s -m pip install pytest"
            % (sys.executable, sys.executable),
        )
        return 2
    os.environ["RUSTPY_TEST_ROOT"] = root
    full = ["--rootdir", root, "--capture=sys", "-p", "rustpy_pytest_plugin"] + list(args)
    code = pytest.main(full)
    try:
        return int(code)
    except Exception:
        return 1


# --------------------------------------------------------------------------
# unittest
# --------------------------------------------------------------------------


def unittest_node_id(test, root):
    address = getattr(test, "address", None)
    if callable(address):
        try:
            file, module, member = address()
            if file:
                return rel(file, root) + ("::" + member.replace(".", "::") if member else "")
        except Exception:
            pass
    cls = type(test)
    method = getattr(test, "_testMethodName", None) or "runTest"
    if cls.__module__ == "unittest.loader":
        # _FailedTest / ModuleImportFailure: the method name is the failed target.
        return method
    module = sys.modules.get(cls.__module__)
    file = getattr(module, "__file__", None)
    if not file:
        try:
            file = inspect.getfile(cls)
        except Exception:
            file = cls.__module__.replace(".", "/") + ".py"
    return "%s::%s::%s" % (rel(file, root), cls.__qualname__.replace(".", "::"), method)


def test_line(test):
    try:
        fn = getattr(type(test), test._testMethodName)
        fn = inspect.unwrap(fn)
        code = getattr(fn, "__code__", None)
        return code.co_firstlineno if code else None
    except Exception:
        return None


def iter_tests(suite):
    for t in suite:
        if isinstance(t, unittest.TestSuite):
            yield from iter_tests(t)
        else:
            yield t


class UnittestReporter(unittest.TestResult):
    def __init__(self, root):
        super().__init__()
        self.root = root
        self.counts = {"passed": 0, "failed": 0, "skipped": 0, "errors": 0}
        self._current = None
        self._t0 = 0.0
        self._bufs = None
        self._orig = None

    def startTest(self, test):
        super().startTest(test)
        self._current = {"outcome": "passed", "message": "", "traceback": "", "expected": None, "actual": None}
        CAPTURED["value"] = None
        self._t0 = time.perf_counter()
        self._orig = (sys.stdout, sys.stderr)
        self._bufs = (io.StringIO(), io.StringIO())
        sys.stdout, sys.stderr = self._bufs
        emit(event="start", id=unittest_node_id(test, self.root))

    def stopTest(self, test):
        super().stopTest(test)
        if self._orig:
            sys.stdout, sys.stderr = self._orig
        out, err = (b.getvalue() for b in self._bufs) if self._bufs else ("", "")
        cur = self._current or {"outcome": "passed", "message": "", "traceback": ""}
        key = cur["outcome"] if cur["outcome"] != "error" else "errors"
        self.counts[key] = self.counts.get(key, 0) + 1
        emit(
            event="result",
            id=unittest_node_id(test, self.root),
            outcome=cur["outcome"],
            duration=time.perf_counter() - self._t0,
            message=cur["message"],
            traceback=cur["traceback"],
            stdout=out,
            stderr=err,
            expected=cur.get("expected"),
            actual=cur.get("actual"),
        )
        self._current = None
        self._bufs = None
        self._orig = None

    def _note_comparison(self, err):
        found = captured_for(err[1] if err else None)
        if found is not None and self._current is not None:
            self._current["expected"], self._current["actual"] = found

    def _set(self, outcome, message, tb=""):
        cur = self._current
        if cur is None:
            return
        # Keep the most severe outcome; append messages for subtests.
        rank = {"passed": 0, "skipped": 1, "failed": 2, "error": 3}
        if rank.get(outcome, 0) >= rank.get(cur["outcome"], 0):
            cur["outcome"] = outcome
        cur["message"] = (cur["message"] + "\n" + message).strip() if cur["message"] else message
        if tb:
            cur["traceback"] = (cur["traceback"] + "\n" + tb).strip() if cur["traceback"] else tb

    def _exc_message(self, err):
        try:
            exc = err[1]
            text = "%s: %s" % (type(exc).__name__, exc) if str(exc) else type(exc).__name__
            return text.splitlines()[0] if text else type(exc).__name__
        except Exception:
            return "error"

    def addError(self, test, err):
        debug_failure(err)
        super().addError(test, err)
        self._set("error", self._exc_message(err), self._exc_info_to_string(err, test))

    def addFailure(self, test, err):
        debug_failure(err)
        super().addFailure(test, err)
        self._set("failed", self._exc_message(err), self._exc_info_to_string(err, test))
        self._note_comparison(err)

    def addSubTest(self, test, subtest, err):
        super().addSubTest(test, subtest, err)
        parent = unittest_node_id(test, self.root)
        description = getattr(subtest, "_subDescription", lambda: str(subtest))()
        child = parent + "::subTest" + description
        emit(event="collected", id=child, name="subTest " + description, line=test_line(test))
        emit(event="start", id=child)
        outcome = "passed" if err is None else ("failed" if issubclass(err[0], test.failureException) else "error")
        emit(event="result", id=child, outcome=outcome, duration=0.0,
             message=self._exc_message(err) if err else "",
             traceback=self._exc_info_to_string(err, test) if err else "", stdout="", stderr="")
        if err is not None:
            debug_failure(err)
            outcome = "failed" if issubclass(err[0], test.failureException) else "error"
            self._set(outcome, "%s: %s" % (subtest.id(), self._exc_message(err)), self._exc_info_to_string(err, test))
            self._note_comparison(err)

    def addSkip(self, test, reason):
        super().addSkip(test, reason)
        self._set("skipped", str(reason))

    def addExpectedFailure(self, test, err):
        super().addExpectedFailure(test, err)
        self._set("skipped", "expected failure: " + self._exc_message(err))

    def addUnexpectedSuccess(self, test):
        super().addUnexpectedSuccess(test)
        self._set("failed", "unexpected success")


SKIP_DIRS = {
    "__pycache__", "node_modules", ".git", ".hg", ".svn", "venv", ".venv", "env", ".env", ".mypy_cache",
    ".pytest_cache", ".ruff_cache", ".tox", "build", "dist", "target", ".idea", ".rustpy", "site-packages", ".eggs",
}


def is_test_file(relpath):
    name = os.path.basename(relpath)
    if not name.endswith(".py") or name in ("conftest.py", "__init__.py"):
        return False
    if name.startswith("test_") or name.endswith("_test.py") or name == "tests.py":
        return True
    parts = relpath.replace("\\", "/").split("/")[:-1]
    return any(p in ("tests", "test", "testing") for p in parts)


MODULE_FILE = re.compile(r"[_a-z]\w*\.py$", re.IGNORECASE)


def walk_test_files(directory, root, pattern=None):
    """Test modules under ``directory``: the files whose names match the glob
    ``pattern`` (``-p``) when one is given, else those ``is_test_file`` accepts."""
    out = []
    for dirpath, dirnames, filenames in os.walk(directory):
        dirnames[:] = sorted(d for d in dirnames if d not in SKIP_DIRS and not d.startswith("."))
        for f in sorted(filenames):
            full = os.path.join(dirpath, f)
            if pattern:
                wanted = f != "__init__.py" and MODULE_FILE.match(f) is not None and fnmatch.fnmatch(f, pattern)
            else:
                wanted = is_test_file(rel(full, root))
            if wanted:
                out.append(full)
    return out


def import_test_file(path):
    """Imports a test module by file path the way pytest's ``prepend`` mode does:
    the first ancestor directory without ``__init__.py`` is put on ``sys.path``
    and the module is imported by its package-qualified name."""
    import importlib

    path = os.path.abspath(path)
    directory = os.path.dirname(path)
    parts = [os.path.splitext(os.path.basename(path))[0]]
    while os.path.isfile(os.path.join(directory, "__init__.py")):
        parts.insert(0, os.path.basename(directory))
        parent = os.path.dirname(directory)
        if parent == directory:
            break
        directory = parent
    if parts[-1] == "__init__":
        parts.pop()
    if directory not in sys.path:
        sys.path.insert(0, directory)
    modname = ".".join(parts)
    existing = sys.modules.get(modname)
    if existing is not None and os.path.abspath(getattr(existing, "__file__", "") or "") != path:
        del sys.modules[modname]
    return importlib.import_module(modname)


def to_unittest_target(target, root):
    """Returns ("dir", path), ("file", path, rest) or ("name", dotted_name)."""
    file_part, _sep, rest = target.partition("::")
    file_part = file_part.replace("\\", "/")
    if file_part.endswith(".py") or "/" in file_part or os.path.exists(os.path.join(root, file_part)):
        p = file_part if os.path.isabs(file_part) else os.path.join(root, file_part)
        p = os.path.abspath(p)
        if os.path.isdir(p):
            return ("dir", p, None)
        return ("file", p, rest.replace("::", ".") if rest else None)
    mod = file_part
    if rest:
        mod = mod + "." + rest.replace("::", ".")
    return ("name", mod, None)


def load_error(target, message):
    emit(
        event="result",
        id=target,
        outcome="error",
        duration=0.0,
        message=message,
        traceback=traceback.format_exc(),
        stdout="",
        stderr="",
    )


def load_file_tests(loader, path, rest, target, root, suites):
    try:
        module = import_test_file(path)
    except BaseException as e:  # noqa: BLE001 - report any import-time failure
        load_error(target, "cannot import %s: %s: %s" % (rel(path, root), type(e).__name__, e))
        return 1
    try:
        if rest:
            suites.append(loader.loadTestsFromName(rest, module))
        else:
            suites.append(loader.loadTestsFromModule(module))
    except Exception as e:
        load_error(target, "cannot load %s: %s: %s" % (target, type(e).__name__, e))
        return 1
    return 0


# Options of `python -m unittest [discover]` that take a value, and where it goes.
UNITTEST_VALUE_OPTIONS = {
    "-k": "patterns",
    "-s": "start",
    "--start-directory": "start",
    "-p": "pattern",
    "--pattern": "pattern",
    "-t": "top",
    "--top-level-directory": "top",
    "--durations": None,
}


def parse_unittest_args(args):
    """Reads ``python -m unittest`` arguments: test names (targets), ``-k``
    name patterns, ``-f``, and the arguments of ``python -m unittest
    discover``: ``-s``/``-p``/``-t`` (start directory, file pattern,
    top-level directory) or, after the word ``discover``, up to three
    positional values in that order. The IDE passes its targets before the
    configuration's arguments. Other flags (``-v``, ``-b``, ``--locals``…)
    do not affect the JSON stream."""
    opts = {"targets": [], "patterns": [], "failfast": False, "discover": False, "start": None, "pattern": None, "top": None}
    positional = None  # discovery values a `discover` word still accepts
    i = 0
    while i < len(args):
        a = args[i]
        i += 1
        name, value = a, None
        if a.startswith("--") and "=" in a:
            name, value = a.split("=", 1)
        elif len(a) > 2 and a[:2] in ("-k", "-s", "-p", "-t"):
            name, value = a[:2], a[2:]
        if name in UNITTEST_VALUE_OPTIONS:
            if value is None:
                if i >= len(args):
                    break
                value = args[i]
                i += 1
            key = UNITTEST_VALUE_OPTIONS[name]
            if key == "patterns":
                opts["patterns"].append(value)
            elif key is not None:
                opts[key] = value
                opts["discover"] = True
        elif a in ("-f", "--failfast") or (len(a) > 2 and a[0] == "-" and a[1] != "-" and "f" in a and set(a[1:]) <= set("fcbvq")):
            opts["failfast"] = True
        elif a.startswith("-") and a != "-":
            pass
        elif a == "discover" and positional is None:
            opts["discover"] = True
            positional = ["start", "pattern", "top"]
        elif positional:
            opts[positional.pop(0)] = a
        else:
            opts["targets"].append(a)
    return opts


def run_unittest(root, args):
    opts = parse_unittest_args(args)
    targets = opts["targets"]
    failfast = opts["failfast"]
    loader = unittest.TestLoader()
    if opts["patterns"]:
        loader.testNamePatterns = [p if any(ch in p for ch in "*?[") else "*%s*" % p for p in opts["patterns"]]
    suites = []
    load_errors = 0
    if not targets and opts["discover"]:
        # As `python -m unittest discover`: unittest's own discovery from the
        # working directory (or -s) with its default file pattern.
        start = opts["start"] or "."
        try:
            suites.append(loader.discover(start, pattern=opts["pattern"] or "test*.py", top_level_dir=opts["top"]))
        except Exception as e:
            load_error(start, "cannot discover tests in %s: %s: %s" % (start, type(e).__name__, e))
            load_errors += 1
    for t in targets or ([] if opts["discover"] else [root]):
        kind, value, rest = to_unittest_target(t, root)
        if kind == "dir":
            for f in walk_test_files(value, root, opts["pattern"]):
                load_errors += load_file_tests(loader, f, None, rel(f, root), root, suites)
        elif kind == "file":
            load_errors += load_file_tests(loader, value, rest, t, root, suites)
        else:
            try:
                suites.append(loader.loadTestsFromName(value))
            except Exception as e:
                load_error(t, "cannot load %s: %s: %s" % (value, type(e).__name__, e))
                load_errors += 1
    seen = set()
    tests = []
    for s in suites:
        for test in iter_tests(s):
            key = test.id()
            if key in seen:
                continue
            seen.add(key)
            tests.append(test)
    for test in tests:
        if type(test).__module__ == "unittest.loader":
            continue
        try:
            file = inspect.getfile(type(test))
        except Exception:
            file = None
        emit(
            event="collected",
            id=unittest_node_id(test, root),
            name=getattr(test, "_testMethodName", str(test)),
            file=rel(file, root) if file else None,
            line=test_line(test),
        )
    result = UnittestReporter(root)
    result.failfast = failfast
    started = time.perf_counter()
    suite = unittest.TestSuite(tests)
    suite.run(result)
    counts = dict(result.counts)
    counts["errors"] += load_errors
    ok = result.wasSuccessful() and load_errors == 0
    emit(event="done", duration=time.perf_counter() - started, exit_code=0 if ok else 1, **counts)
    return 0 if ok else 1


def main(argv):
    framework = "pytest"
    root = os.getcwd()
    cwd = None
    rest = []
    i = 0
    while i < len(argv):
        a = argv[i]
        if a == "--framework" and i + 1 < len(argv):
            framework = argv[i + 1]
            i += 2
        elif a == "--root" and i + 1 < len(argv):
            root = argv[i + 1]
            i += 2
        elif a == "--cwd" and i + 1 < len(argv):
            cwd = argv[i + 1]
            i += 2
        elif a == "--":
            rest = argv[i + 1 :]
            break
        else:
            rest.append(a)
            i += 1
    root = os.path.abspath(root)
    if root not in sys.path:
        sys.path.insert(0, root)
    try:
        # Tests run in the configured working directory; node ids stay relative to the root.
        os.chdir(os.path.abspath(cwd) if cwd else root)
    except OSError:
        pass
    pytest_path = os.environ.get("RUSTPY_PYTEST_PATH", "").strip()
    if pytest_path:
        sys.path.insert(0, os.path.dirname(pytest_path) if os.path.basename(pytest_path) == "pytest" else pytest_path)
    install_equality_capture()
    if framework in ("nose", "nose2", "lettuce", "tox"):
        import rustpy_legacy_reporter
        return rustpy_legacy_reporter.run(framework, root, rest, globals())
    if framework == "doctest":
        import runpy
        adapter = runpy.run_path(os.path.join(os.path.dirname(__file__), "rustpy_doctest_reporter.py"))
        return adapter["run_doctest"](root, rest, emit, import_test_file)
    if framework == "unittest":
        return run_unittest(root, rest)
    return run_pytest(root, rest)


if __name__ == "__main__":
    try:
        code = main(sys.argv[1:])
    except SystemExit as e:
        code = e.code if isinstance(e.code, int) else 1
    except BaseException:
        emit(event="error", message=traceback.format_exc())
        code = 3
    sys.stdout.flush()
    sys.exit(code)
