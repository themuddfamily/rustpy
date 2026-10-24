"""RustPy Python Console helper.

Speaks a JSON-lines protocol with the IDE:

* requests arrive on stdin, one JSON object per line:
  ``{"id": 1, "op": "exec", "code": "..."}``,
  ``{"id": 2, "op": "complete", "code": "os.pa", "cursor": 5}``,
  ``{"id": 3, "op": "vars", "include_modules": false}``,
  ``{"id": 4, "op": "inspect", "name": "x", "max_items": 100}``,
  ``{"id": 5, "op": "exec", "code": "...", "mode": "exec"}`` (a fragment sent
  from the editor: run as a whole, never waits for more input),
  ``{"id": 6, "op": "runfile", "path": "/p/a.py", "args": [], "wdir": "/p",
  "env": {}}`` (run a script in the console namespace),
  ``{"op": "startup", "code": "..."}`` (run silently), ``{"op": "exit"}``.
* protocol messages go to stdout as ``##rustpy-console## {json}`` lines;
  anything the user's code writes straight to the file descriptors is
  forwarded by the IDE as plain output.

Only the standard library is used so the helper works with any interpreter.
"""

import ast
import builtins
import codeop
import io
import json
import keyword
import os
import shlex
import signal
import sys
import threading
import traceback
import types
import base64
# The sibling helper modules live in a temporary directory the IDE removes;
# keep their bytecode out of it without changing the setting for user code.
_write_bytecode, sys.dont_write_bytecode = sys.dont_write_bytecode, True
from console_values import Values, execute, safe_repr
from rustpy_scientific_table import scientific_table as _scientific_table
from rustpy_scientific_plots import scientific_install, scientific_patch_interactive, scientific_capture
sys.dont_write_bytecode = _write_bytecode
del _write_bytecode

PREFIX = "##rustpy-console## "
_OUT = sys.__stdout__
_IN = sys.__stdin__
_LOCK = threading.Lock()
_HELPER_FILE = __file__
_VALUE_FILE = execute.__code__.co_filename
_HELPER_MODULE = None


def send(msg):
    with _LOCK:
        _OUT.write(PREFIX + json.dumps(msg) + "\n")
        _OUT.flush()


class StreamWriter(io.TextIOBase):
    """Captures user stdout/stderr and streams it as protocol events."""

    def __init__(self, event):
        super().__init__()
        self.event = event
        self.buf = []
        self.request_id = None

    @property
    def encoding(self):
        return "utf-8"

    def writable(self):
        return True

    def isatty(self):
        return False

    def write(self, s):
        if not isinstance(s, str):
            s = str(s)
        self.buf.append(s)
        if "\n" in s:
            self.flush()
        return len(s)

    def flush(self):
        if self.buf:
            text = "".join(self.buf)
            self.buf = []
            send({"id": self.request_id, "event": self.event, "text": text})


class NoInput(io.TextIOBase):
    """``input()`` in the console raises EOFError (stdin carries the protocol)."""

    def readable(self):
        return True

    def read(self, size=-1):
        return ""

    def readline(self, size=-1):
        return ""


STDOUT = StreamWriter("stdout")
STDERR = StreamWriter("stderr")
# The user namespace is the dict of a real module that main() registers as
# ``sys.modules["__main__"]``, so pickle, dataclasses and typing.get_type_hints
# find classes and functions defined in the console or by runfile().
MAIN_MODULE = types.ModuleType("__main__")
NAMESPACE = MAIN_MODULE.__dict__
NAMESPACE.update({"__doc__": None, "__builtins__": builtins})
VALUES = Values(NAMESPACE, scientific_table=_scientific_table)
IPYTHON = None
CAPTURE_PLOTS = os.environ.get("RUSTPY_SCIENTIFIC_CAPTURE") == "1"
if CAPTURE_PLOTS:
    scientific_install(True)
DEBUG_ENDPOINT = None


def _strip_helper_frames(te, seen):
    """Removes this helper's frames from a TracebackException and its chain."""
    while te is not None and id(te) not in seen:
        seen.add(id(te))
        frames = [f for f in te.stack if f.filename not in (_HELPER_FILE, _VALUE_FILE)]
        te.stack = traceback.StackSummary.from_list(frames)
        if not frames and hasattr(te, "exc_traceback"):
            te.exc_traceback = None  # Python < 3.11 prints the header without frames
        for sub in getattr(te, "exceptions", None) or ():
            _strip_helper_frames(sub, seen)
        _strip_helper_frames(te.__context__, seen)
        te = te.__cause__


def format_traceback(skip_helper=True):
    etype, value, tb = sys.exc_info()
    if skip_helper:
        # A fragment is parsed by ast.parse, unlike typed-input codeop. Only
        # compiler-owned SyntaxError traces are presentation-free; a user-raised
        # SyntaxError or one from compile() inside user code keeps its frames.
        compiler_files = (_HELPER_FILE, _VALUE_FILE, ast.__file__, codeop.__file__)
        if isinstance(value, SyntaxError) and all(
            frame.filename in compiler_files for frame in traceback.extract_tb(tb)
        ):
            return "".join(traceback.format_exception_only(etype, value))
        # Drop frames that belong to this helper (the exec/eval/runfile call sites).
        try:
            te = traceback.TracebackException(etype, value, tb)
            _strip_helper_frames(te, set())
            return "".join(te.format())
        except Exception:  # noqa: BLE001 - fall back to the plain leading-frame filter
            while tb is not None and tb.tb_frame.f_code.co_filename == _HELPER_FILE:
                tb = tb.tb_next
    if tb is None:
        return "".join(traceback.format_exception_only(etype, value))
    return "".join(traceback.format_exception(etype, value, tb))


def compile_console(code):
    """Returns ``(code_object, more)``; raises SyntaxError for bad input.

    Single-statement input follows the REPL rules (a block stays open until a
    blank line); multi-statement input (pasted blocks) uses exec-mode rules.
    Depending on the Python version a pasted block such as ``for …: …`` followed
    by ``x = 1`` fails in single mode with "invalid syntax" rather than
    "multiple statements", so every multi-line input gets the exec-mode retry.
    """
    try:
        compiled = codeop.compile_command(code, "<console>", "single")
        return compiled, compiled is None
    except SyntaxError as exc:
        if "multiple statements" not in str(exc) and "\n" not in code.strip():
            raise
    compiled = codeop.compile_command(code, "<console>", "exec")
    return compiled, compiled is None


def run_code(code):
    """Executes ``code`` in the namespace; returns the repr of a trailing expression."""
    if IPYTHON is not None:
        result = IPYTHON.run_cell(code, store_history=True)
        if result.error_before_exec or result.error_in_exec:
            raise result.error_before_exec or result.error_in_exec
        return safe_repr(result.result, 4000) if result.result is not None else None
    result = execute(code, NAMESPACE)
    return safe_repr(result, 4000) if result is not None else None


def run_reporting(func):
    """Runs ``func()``; returns ``(value, error)``, writing tracebacks to stderr."""
    try:
        return func(), None
    except SystemExit as exc:
        error = "SystemExit: %s" % (exc.code,)
        STDERR.write(error + "\n")
    except KeyboardInterrupt:
        error = "KeyboardInterrupt"
        STDERR.write("KeyboardInterrupt\n")
    except BaseException:  # noqa: BLE001 - report everything, keep running
        error = format_traceback()
        STDERR.write(error)
    return None, error


def do_exec(req, silent=False):
    """``mode`` "single" (default): typed input, REPL completeness rules.
    ``mode`` "exec": a fragment from the editor (selection, line, cell) that runs
    as a whole even when it ends with a compound statement; incomplete code is
    a SyntaxError instead of a ``...`` prompt."""
    rid = req.get("id")
    code = req.get("code") or ""
    STDOUT.request_id = STDERR.request_id = rid
    value = None
    error = None
    more = False
    try:
        if req.get("mode") == "exec":
            value, error = run_reporting(lambda: run_code(code))
        elif IPYTHON is not None:
            status, _ = IPYTHON.input_transformer_manager.check_complete(code)
            more = status == 'incomplete'
            if not more:
                value, error = run_reporting(lambda: run_code(code))
        else:
            try:
                _, more = compile_console(code)
            except SyntaxError:
                error = "".join(traceback.format_exception_only(*sys.exc_info()[:2]))
                STDERR.write(error)
            else:
                if not more:
                    value, error = run_reporting(lambda: run_code(code))
    finally:
        if not more:
            VALUES.changed()
            capture_plots(rid)
        STDOUT.flush()
        STDERR.flush()
    if not silent:
        send({"id": rid, "event": "result", "value": value, "error": error, "more": more})


_MISSING = object()


def runfile(filename, args=None, wdir=None, env=None, inherit_environment=True):
    """Runs a script in the console namespace (the ``runfile`` console helper).

    ``__file__`` and ``sys.argv`` (``[filename] + args``; ``args`` may be a
    string) are set for the run, ``wdir`` becomes the working directory and
    ``env`` is applied to ``os.environ``. Variables the script defines stay in
    the console. The script's directory is put on ``sys.path`` as
    ``python script.py`` would.
    """
    if wdir:
        os.chdir(wdir)
    filename = os.path.abspath(filename)
    if isinstance(args, str):
        args = shlex.split(args)
    with open(filename, "rb") as f:
        source = f.read()
    code = compile(source, filename, "exec", dont_inherit=True)
    script_dir = os.path.dirname(filename)
    if script_dir not in sys.path:
        sys.path.insert(0, script_dir)
    old_file = NAMESPACE.get("__file__", _MISSING)
    old_argv = sys.argv
    NAMESPACE["__file__"] = filename
    sys.argv = [filename] + [str(a) for a in (args or ())]
    old_environment = dict(os.environ) if not inherit_environment else None
    try:
        if old_environment is not None:
            os.environ.clear()
        for key, val in (env or {}).items():
            os.environ[str(key)] = str(val)
        exec(code, NAMESPACE)
    finally:
        if old_environment is not None:
            os.environ.clear()
            os.environ.update(old_environment)
        sys.argv = old_argv
        if old_file is _MISSING:
            NAMESPACE.pop("__file__", None)
        else:
            NAMESPACE["__file__"] = old_file


def do_runfile(req):
    rid = req.get("id")
    STDOUT.request_id = STDERR.request_id = rid
    error = None
    try:
        _, error = run_reporting(lambda: runfile(req.get("path") or "", req.get("args") or [], req.get("wdir"), req.get("env") or {}, req.get("inherit_environment", True)))
    finally:
        VALUES.changed()
        capture_plots(rid)
        STDOUT.flush()
        STDERR.flush()
    send({"id": rid, "event": "result", "value": None, "error": error, "more": False})


def kind_of(obj):
    if isinstance(obj, types.ModuleType):
        return "module"
    if isinstance(obj, type):
        return "class"
    if isinstance(obj, (types.MethodType, types.MethodDescriptorType, types.WrapperDescriptorType, types.ClassMethodDescriptorType)):
        return "method"
    if isinstance(obj, types.FunctionType):
        return "function"
    if isinstance(obj, types.BuiltinFunctionType):
        # builtins such as print have the module as __self__; bound builtin methods have an instance.
        owner = getattr(obj, "__self__", None)
        return "function" if owner is None or isinstance(owner, types.ModuleType) else "method"
    if callable(obj):
        return "function"
    return "variable"


def completion_target(code, cursor):
    """The dotted-name prefix ending at ``cursor``."""
    head = code[:cursor]
    i = len(head)
    while i > 0 and (head[i - 1].isalnum() or head[i - 1] in "._"):
        i -= 1
    return head[i:]


def completion_name(match):
    # rlcompleter appends either '(' or '()' to callable names (the latter
    # for zero-argument methods such as str.upper). The UI replaces a name,
    # not a call expression, and kind lookup must inspect the uncalled value.
    if match.endswith("()"):
        return match[:-2]
    return match[:-1] if match.endswith("(") else match


def do_complete(req):
    rid = req.get("id")
    code = req.get("code") or ""
    cursor = req.get("cursor")
    if cursor is None:
        cursor = len(code)
    else:
        # ConsoleSession::complete uses UTF-8 byte offsets. Clamp to the
        # captured source and round an interior code-point offset down, then
        # supply Python/IPython with their required character position.
        encoded = code.encode("utf-8")
        cursor = len(encoded[:max(0, min(cursor, len(encoded)))].decode("utf-8", errors="ignore"))
    text = completion_target(code, cursor)
    prefix = text.rpartition(".")[2]
    items = []
    seen = set()
    try:
        if IPYTHON is not None:
            # The public shell API supplies a single, common replacement
            # fragment. Its offsets, like Python slices, count Unicode characters.
            # Supply the physical line containing the captured request cursor,
            # including text after it, rather than completing the current input.
            line_start = code.rfind("\n", 0, cursor) + 1
            line_end = code.find("\n", cursor)
            line = code[line_start:line_end if line_end >= 0 else len(code)]
            # IPython's common-fragment API deliberately excludes Jedi results,
            # while use_jedi=True also removes its Python namespace matcher.
            # Select the compatible Python matcher for this serialized request
            # only; leave the user's interactive completion preference intact.
            completer = IPYTHON.Completer
            use_jedi = completer.use_jedi
            try:
                completer.use_jedi = False
                fragment, candidates = IPYTHON.complete("", line, cursor - line_start)
            finally:
                completer.use_jedi = use_jedi
            head = code[line_start:cursor]
            # Our protocol replaces only the final identifier. Never widen that
            # edit for IPython path/string/magic matchers with different spans.
            if not head.endswith(fragment) or not fragment.endswith(prefix):
                candidates = []
            preserved = fragment[:len(fragment) - len(prefix)] if prefix else fragment
            matches = []
            for candidate in candidates[:501]:
                if candidate.startswith(preserved):
                    suffix = candidate[len(preserved):]
                    if suffix.startswith(prefix):
                        matches.append((suffix, candidate))
        else:
            import rlcompleter

            completer = rlcompleter.Completer(NAMESPACE)
            matches = []
            for state in range(501):
                if not text:
                    break
                match = completer.complete(text, state)
                if match is None:
                    break
                matches.append((match.rpartition(".")[2], match))
        for replacement, match in matches:
            name = completion_name(match)
            kind = "variable"
            if match.startswith("%"):
                kind = "function"
            elif "." in text:
                parent_expr, _, attr = name.rpartition(".")
                try:
                    parent = eval(parent_expr, NAMESPACE)
                    kind = kind_of(getattr(parent, attr))
                except Exception:  # noqa: BLE001
                    kind = "attribute"
            elif keyword.iskeyword(name):
                kind = "keyword"
            else:
                try:
                    obj = NAMESPACE[name] if name in NAMESPACE else getattr(builtins, name)
                    kind = kind_of(obj)
                except Exception:  # noqa: BLE001
                    kind = "variable"
            name = completion_name(replacement)
            if name and name not in seen:
                seen.add(name)
                items.append({"name": name, "kind": kind})
    except Exception:  # noqa: BLE001
        pass
    items.sort(key=lambda it: (it["name"].startswith("_"), it["name"].lower()))
    send({"id": rid, "event": "completions", "items": items, "prefix": prefix})


def do_vars(req):
    page = VALUES.variables(0, 200, bool(req.get('include_modules', False)))
    send(dict(id=req.get('id'), event='vars', items=page['items']))


def do_inspect(req):
    response = VALUES.dispatch(dict(req, op='page', count=req.get('max_items', 100)))
    response['event'] = 'children'
    send(response)


def capture_plots(rid):
    if not CAPTURE_PLOTS:
        return
    try:
        scientific_patch_interactive()
        for plot in scientific_capture():
            send(dict(id=rid, event='plot', **plot))
    except BaseException:
        STDERR.write(format_traceback())


def configure(req):
    global IPYTHON, CAPTURE_PLOTS
    error = None
    try:
        if 'ipython' in req:
            if req['ipython']:
                from IPython.core.interactiveshell import InteractiveShell
                IPYTHON = InteractiveShell(user_ns=NAMESPACE)
            else:
                IPYTHON = None
        if 'interactive_plots' in req:
            os.environ['RUSTPY_SCIENTIFIC_INTERACTIVE'] = '1' if req['interactive_plots'] else '0'
        if 'plots' in req:
            CAPTURE_PLOTS = bool(req['plots'])
            scientific_install(CAPTURE_PLOTS)
            if CAPTURE_PLOTS:
                # Select a headless backend before pyplot is imported.
                os.environ.setdefault('MPLBACKEND', 'Agg')
                pyplot = sys.modules.get('matplotlib.pyplot')
                if pyplot is not None:
                    pyplot.switch_backend('Agg')
    except BaseException as exc:
        error = '%s: %s' % (type(exc).__name__, exc)
    send(dict(id=req.get('id'), event='configured', ipython=IPYTHON is not None, plots=CAPTURE_PLOTS, error=error))


def debugger(req):
    global DEBUG_ENDPOINT
    try:
        if DEBUG_ENDPOINT is None:
            import debugpy
            DEBUG_ENDPOINT = debugpy.listen(('127.0.0.1', 0))
        host, port = DEBUG_ENDPOINT
        send(dict(id=req.get('id'), event='debugger', host=host, port=port, error=None))
    except BaseException as exc:
        send(dict(id=req.get('id'), event='debugger', host='', port=0, error='%s: %s' % (type(exc).__name__, exc)))


def main():
    global _HELPER_MODULE
    # Keep this script's own module alive once the user namespace takes its place.
    _HELPER_MODULE = sys.modules.get("__main__")
    sys.modules["__main__"] = MAIN_MODULE
    builtins.runfile = runfile
    sys.argv = ["<console>"]
    cwd = os.getcwd()
    if sys.path and sys.path[0] != cwd:
        sys.path[0] = cwd
    if cwd not in sys.path:
        sys.path.insert(0, cwd)
    sys.stdout = STDOUT
    sys.stderr = STDERR
    sys.stdin = NoInput()
    signal.signal(signal.SIGINT, signal.default_int_handler)
    if hasattr(signal, 'SIGBREAK'):
        signal.signal(signal.SIGBREAK, signal.default_int_handler)
    send({"event": "ready", "version": sys.version.split()[0], "executable": sys.executable})
    while True:
        try:
            line = _IN.readline()
        except KeyboardInterrupt:
            continue
        if not line:
            break
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except ValueError:
            send({"event": "stderr", "id": None, "text": "console: bad request %r\n" % (line[:80],)})
            continue
        op = req.get("op")
        try:
            if op == "exec":
                do_exec(req)
            elif op == "startup":
                do_exec(req, silent=True)
            elif op == "runfile":
                do_runfile(req)
            elif op == "complete":
                do_complete(req)
            elif op == "vars":
                do_vars(req)
            elif op == "inspect":
                do_inspect(req)
            elif op in ('page', 'vars_page', 'set', 'data'):
                send(VALUES.dispatch(req))
            elif op == 'configure':
                configure(req)
            elif op == 'debugger':
                debugger(req)
            elif op == "exit":
                break
            else:
                send({"id": req.get("id"), "event": "stderr", "text": "console: unknown op %r\n" % (op,)})
        except KeyboardInterrupt:
            send({"id": req.get("id"), "event": "result", "value": None, "error": "KeyboardInterrupt", "more": False})
        except BaseException:
            # A hostile value/completer must not terminate the transport loop.
            send(dict(id=req.get('id'), event='stderr', text=format_traceback()))
    STDOUT.flush()
    STDERR.flush()


if __name__ == "__main__":
    main()
