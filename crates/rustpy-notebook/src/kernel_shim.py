"""RustPy notebook kernel shim.

The notebook runner (``rustpy-notebook``) installs this into the Python
Console helper process (``rustpy-console``) through its startup code. It is
executed in a private namespace that provides ``_USER_NS`` (the console's user
namespace) and ``_TOKEN`` (a random marker token chosen by the runner), and it
binds ``__rustpy_nb__`` in the user namespace.

``__rustpy_nb__.run(count, source)`` executes one notebook cell the way
IPython does: the whole cell is compiled as a module (so compound statements
need no trailing blank line), the value of a trailing expression is displayed
unless the cell ends with ``;``, and exceptions are reported without the
frames of this shim. Common IPython magics are emulated (``magic`` and
``_cell_magic``); other magics are reported as a ``UsageError``.

Rich results go to the IDE as single lines on the console's stdout stream:
``##rustpy-nb-<token>## {json}``, where the JSON object has a ``type`` of
``execute_result`` / ``display_data`` (``data``: mime type -> text,
``metadata``) or ``error`` (``ename``, ``evalue``, ``traceback`` lines).

``sys.modules["__main__"]`` becomes a module whose attributes are the user
namespace (as in IPython), so ``pickle`` and ``multiprocessing`` find the
functions and classes defined in cells.

Only the standard library is used.
"""

import ast
import base64
import builtins
import codecs
import io
import json
import linecache
import os
import shlex
import subprocess
import sys
import time
import tokenize
import traceback
import types
import warnings

_PREFIX = "##rustpy-nb-%s## " % (_TOKEN,)  # noqa: F821 - provided by the runner
_OUT = sys.stdout  # the console helper's stdout stream (survives user redirection)
_REPR_METHODS = (
    ("text/html", "_repr_html_"),
    ("text/markdown", "_repr_markdown_"),
    ("text/latex", "_repr_latex_"),
    ("image/svg+xml", "_repr_svg_"),
    ("image/png", "_repr_png_"),
    ("image/jpeg", "_repr_jpeg_"),
    ("application/json", "_repr_json_"),
    ("application/javascript", "_repr_javascript_"),
)
# Cell magics that show the cell body as a rich output of the given type.
_DISPLAY_CELL_MAGICS = {
    "html": "text/html",
    "markdown": "text/markdown",
    "latex": "text/latex",
    "svg": "image/svg+xml",
    "javascript": "application/javascript",
    "js": "application/javascript",
}
_SUPPORTED_MAGICS = (
    "supported: !command, %pip, %cd, %pwd, %env, %time, %matplotlib, %config, %load_ext autoreload, "
    "%%time, %%writefile, %%bash, %%sh, %%html, %%markdown, %%latex, %%svg, %%javascript"
)
_shown_figures = []  # matplotlib figures displayed by the running cell


class UsageError(Exception):
    """An unsupported or malformed IPython magic."""


def _is_json_mime(mime):
    return mime == "application/json" or (mime.startswith("application/") and mime.endswith("+json"))


def _flush_streams():
    streams = []
    for stream in (sys.stdout, sys.stderr, _OUT):
        if not any(stream is seen for seen in streams):
            streams.append(stream)
    for stream in streams:
        try:
            stream.flush()
        except Exception:  # noqa: BLE001 - user-replaced streams may misbehave
            pass


def _emit(msg):
    """Sends one message to the IDE, after the output written before it."""
    _flush_streams()
    try:
        line = json.dumps(msg, default=str, allow_nan=False)
    except ValueError:  # NaN or a circular structure in user-supplied metadata
        line = json.dumps(dict(msg, metadata={}), default=str)
    _OUT.write(_PREFIX + line + "\n")
    _OUT.flush()


def _mime_text(mime, value):
    """The text stored in the notebook for one representation of a value."""
    if _is_json_mime(mime):
        if isinstance(value, bytes):
            value = value.decode("utf-8", "replace")
        if isinstance(value, str):
            value = json.loads(value)
        return json.dumps(value, default=str)
    if isinstance(value, bytes):
        if mime.startswith("image/") and mime != "image/svg+xml" or mime == "application/pdf":
            return base64.b64encode(value).decode("ascii")
        return value.decode("utf-8", "replace")
    if isinstance(value, str):
        return value
    return str(value)


def _real_method(obj, name):
    """``obj.<name>`` when it is a real bound method (not on classes or catch-all objects)."""
    if isinstance(obj, type):
        return None
    try:
        if hasattr(obj, "_ipython_canary_method_should_not_exist_"):
            return None
        method = getattr(obj, name, None)
    except Exception:  # noqa: BLE001
        return None
    return method if callable(method) else None


def _is_figure(obj):
    module = sys.modules.get("matplotlib.figure")
    figure_type = getattr(module, "Figure", None)
    return isinstance(figure_type, type) and isinstance(obj, figure_type)


def _figure_bundle(fig):
    buf = io.BytesIO()
    fig.savefig(buf, format="png", bbox_inches="tight")
    if not any(fig is shown for shown in _shown_figures):
        _shown_figures.append(fig)
    return {"image/png": base64.b64encode(buf.getvalue()).decode("ascii"), "text/plain": repr(fig)}, {}


def _bundle(obj):
    """``(data, metadata)`` for ``obj``: text/plain plus the rich reprs it offers."""
    if _is_figure(obj):
        return _figure_bundle(obj)
    data = {}
    metadata = {}
    method = _real_method(obj, "_repr_mimebundle_")
    if method is not None:
        try:
            result = method(include=None, exclude=None)
        except Exception:  # noqa: BLE001 - a broken rich repr falls back to the others
            result = None
        if isinstance(result, tuple) and len(result) == 2:
            result, md = result
            if isinstance(md, dict):
                metadata.update(md)
        if isinstance(result, dict):
            for mime, value in result.items():
                try:
                    data[str(mime)] = _mime_text(str(mime), value)
                except Exception:  # noqa: BLE001
                    pass
    for mime, name in _REPR_METHODS:
        if mime in data:
            continue
        method = _real_method(obj, name)
        if method is None:
            continue
        try:
            value = method()
        except Exception:  # noqa: BLE001 - e.g. NotImplementedError
            continue
        if isinstance(value, tuple) and len(value) == 2:
            value, md = value
            if md is not None:
                metadata[mime] = md
        if value is None:
            continue
        try:
            data[mime] = _mime_text(mime, value)
        except Exception:  # noqa: BLE001
            pass
    if "text/plain" not in data:
        data["text/plain"] = repr(obj)
    try:
        module = type(obj).__module__
        if module.startswith("plotly") and hasattr(obj, "to_plotly_json"):
            import plotly.io
            data["text/html"] = plotly.io.to_html(obj, include_plotlyjs=True, full_html=True)
        elif module.startswith("bokeh"):
            from bokeh.embed import file_html
            from bokeh.resources import INLINE
            data["text/html"] = file_html(obj, INLINE)
        elif module.startswith("altair") and hasattr(obj, "to_html"):
            data["text/html"] = obj.to_html(inline=True)
        elif module.startswith("pandas") and hasattr(obj, "columns") and hasattr(obj, "iloc"):
            cache = _USER_NS.setdefault("__rustpy_table_results__", {})  # noqa: F821
            if len(cache) >= 32:
                cache.pop(next(iter(cache)))
            key = str(id(obj))
            cache[key] = obj
            data["application/vnd.rustpy.table+json"] = json.dumps(dict(expression="__rustpy_table_results__[%r]" % key,kind="dataframe",columns=[str(c) for c in obj.columns[:20]],index=[str(i) for i in obj.index[:100]],rows=[[str(v) for v in row] for row in obj.iloc[:100,:20].itertuples(index=False,name=None)],row_start=0,col_start=0,total_rows=len(obj),total_cols=len(obj.columns),truncated=len(obj)>100 or len(obj.columns)>20))
    except Exception:
        pass
    return data, metadata


def display(*objs, **kwargs):
    """Shows each object as a rich output of the running cell (like IPython's ``display``)."""
    raw = kwargs.get("raw", False)
    extra = kwargs.get("metadata")
    for obj in objs:
        if raw and isinstance(obj, dict):
            data = dict((str(k), _mime_text(str(k), v)) for k, v in obj.items())
            metadata = {}
        else:
            data, metadata = _bundle(obj)
        if isinstance(extra, dict):
            metadata.update(extra)
        _emit({"type": "display_data", "data": data, "metadata": metadata})


def _flush_figures():
    """Shows the figures a cell left open (like matplotlib's inline backend), then closes them."""
    pyplot = sys.modules.get("matplotlib.pyplot")
    if pyplot is None:
        return
    try:
        import matplotlib

        if str(matplotlib.get_backend()).lower() != "agg":
            return  # an interactive backend shows its own windows
        for num in pyplot.get_fignums():
            fig = pyplot.figure(num)
            if not any(fig is shown for shown in _shown_figures):
                data, metadata = _figure_bundle(fig)
                _emit({"type": "display_data", "data": data, "metadata": metadata})
        pyplot.close("all")
    except Exception:  # noqa: BLE001 - plotting problems must not break the cell report
        pass


def _quiet(source):
    """True when the cell ends with ``;`` (IPython's rule for hiding the result)."""
    try:
        tokens = list(tokenize.generate_tokens(io.StringIO(source).readline))
    except Exception:  # noqa: BLE001
        return False
    skip = (tokenize.ENDMARKER, tokenize.NL, tokenize.NEWLINE, tokenize.COMMENT, tokenize.INDENT, tokenize.DEDENT)
    for token in reversed(tokens):
        if token[0] in skip:
            continue
        return token[0] == tokenize.OP and token[1] == ";"
    return False


def _shell(command, check=False):
    """Runs ``command`` (a shell command line, or an argument list), streaming its combined
    output to the cell; ``check`` turns a non-zero exit status into ``CalledProcessError``."""
    _flush_streams()
    proc = subprocess.Popen(command, shell=isinstance(command, str), stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    decoder = codecs.getincrementaldecoder("utf-8")("replace")
    try:
        while True:
            chunk = proc.stdout.read1(65536) if hasattr(proc.stdout, "read1") else proc.stdout.readline()
            if not chunk:
                break
            sys.stdout.write(decoder.decode(chunk))
            sys.stdout.flush()
        tail = decoder.decode(b"", True)
        if tail:
            sys.stdout.write(tail)
    except BaseException:
        proc.kill()
        proc.wait()
        raise
    status = proc.wait()
    if check and status != 0:
        raise subprocess.CalledProcessError(status, command if isinstance(command, str) else command[0])


def _quote(path):
    if os.name == "nt":
        return '"%s"' % (path,)
    return shlex.quote(path)


def _unquote(text):
    """``text`` without one pair of surrounding quotes."""
    if len(text) >= 2 and text[0] == text[-1] and text[0] in "'\"":
        return text[1:-1]
    return text


def _format_time(seconds):
    """A duration the way IPython's ``%time`` shows it (``1.23 s``, ``45.6 ms``, ``2min 3s``)."""
    if seconds >= 60.0:
        parts = []
        for suffix, length in (("d", 86400), ("h", 3600), ("min", 60), ("s", 1)):
            value = int(seconds / length)
            if value > 0:
                seconds = seconds % length
                parts.append("%d%s" % (value, suffix))
            if seconds < 1:
                break
        return " ".join(parts)
    for unit, scale in (("s", 1.0), ("ms", 1e3), ("\xb5s", 1e6)):
        if seconds * scale >= 1.0:
            return "%.3g %s" % (seconds * scale, unit)
    return "%.3g ns" % (seconds * 1e9,)


def _cpu_times():
    times = os.times()
    return times[0], times[1]


def _print_times(wall_start, cpu_start):
    """Prints the CPU and wall time since the starting points, like IPython's ``%time``."""
    wall = time.perf_counter() - wall_start
    user, system = (end - start for end, start in zip(_cpu_times(), cpu_start))
    if sys.platform == "win32":
        print("CPU times: total: %s" % (_format_time(user + system),))
    else:
        print("CPU times: user %s, sys: %s, total: %s" % (_format_time(user), _format_time(system), _format_time(user + system)))
    print("Wall time: %s" % (_format_time(wall),))


def _time_statement(statement):
    """``%time statement``: runs it in the user namespace, prints the time taken and returns the
    value when it is an expression."""
    try:
        code, expression = compile(statement, "<timed eval>", "eval"), True
    except SyntaxError:
        code, expression = compile(statement, "<timed exec>", "exec"), False
    wall, cpu = time.perf_counter(), _cpu_times()
    if expression:
        value = eval(code, _USER_NS)  # noqa: F821 - provided by the runner
    else:
        exec(code, _USER_NS)  # noqa: F821
        value = None
    _print_times(wall, cpu)
    return value


def _env(args):
    """``%env``: all variables, one variable's value, or ``%env NAME=value`` (or ``NAME value``)."""
    if not args:
        return dict(os.environ)
    if "=" in args or " " in args:
        name, _, value = args.partition("=" if "=" in args else " ")
        name, value = name.strip(), value.strip()
        if not name or any(c.isspace() for c in name):
            raise UsageError("usage is '%env NAME=value'")
        os.environ[name] = value
        print("env: %s=%s" % (name, value))
        return None
    if args in os.environ:
        return os.environ[args]
    raise UsageError("Environment does not have key: %s" % (args,))


def magic(line):
    """Runs one line magic or shell command; cells call it for their ``%magic`` and ``!command``
    lines. ``%matplotlib``, ``%config`` and ``%autoreload`` are accepted and do nothing (figures
    are always shown inline; IPython configuration does not apply)."""
    if line.startswith("!"):
        _shell(line[1:])
        return None
    name, _, args = line[1:].strip().partition(" ")
    args = args.strip()
    if name in ("matplotlib", "config", "autoreload"):
        return None
    if name in ("load_ext", "reload_ext") and args == "autoreload":
        sys.stderr.write("autoreload is not available in this kernel; restart the kernel to pick up changed modules\n")
        return None
    if name == "pip":
        _shell("%s -m pip %s" % (_quote(sys.executable), args))
        return None
    if name == "cd":
        os.chdir(os.path.expanduser(_unquote(args) or "~"))
        print(os.getcwd())
        return None
    if name == "pwd":
        return os.getcwd()
    if name == "env":
        return _env(args)
    if name == "time":
        return _time_statement(args)
    raise UsageError("Line magic function `%%%s` not found (%s)." % (name, _SUPPORTED_MAGICS))


def _writefile(args, body):
    """``%%writefile [-a] filename``: writes (or appends) the cell body to a file."""
    try:
        words = [_unquote(word) for word in shlex.split(args, posix=os.name != "nt")]
    except ValueError as exc:  # unbalanced quotes
        raise UsageError("%%writefile: " + str(exc))
    append = any(word in ("-a", "--append") for word in words)
    names = [word for word in words if word not in ("-a", "--append")]
    if len(names) != 1:
        raise UsageError("usage: %%writefile [-a] filename")
    filename = os.path.expanduser(names[0])
    if os.path.exists(filename):
        print("%s %s" % ("Appending to" if append else "Overwriting", filename))
    else:
        print("Writing %s" % (filename,))
    with io.open(filename, "a" if append else "w", encoding="utf-8") as f:
        f.write(body)


def _cell_magic(source, filename):
    """Runs a cell that starts with a ``%%magic`` line."""
    blank = len(source) - len(source.lstrip())
    first, _, body = source[blank:].partition("\n")
    name, _, args = first[2:].strip().partition(" ")
    args = args.strip()
    if name == "time":
        # Leading newlines keep the body's line numbers those of the cell.
        _execute("\n" * (source[:blank].count("\n") + 1) + body, filename, timed=True)
    elif name == "writefile":
        _writefile(args, body)
    elif name in ("bash", "sh"):
        _shell([name, "-c", body], check=True)
    elif name in _DISPLAY_CELL_MAGICS:
        _emit({"type": "display_data", "data": {_DISPLAY_CELL_MAGICS[name]: body}, "metadata": {}})
    else:
        raise UsageError("Cell magic `%%%%%s` not found (%s)." % (name, _SUPPORTED_MAGICS))


def _transform_magics(source):
    """Rewrites ``%magic``/``!cmd`` lines into ``__rustpy_nb__.magic(...)`` calls (None if there are none)."""
    lines = source.split("\n")
    changed = False
    for i, line in enumerate(lines):
        stripped = line.lstrip()
        if stripped.startswith(("%", "!")) and not stripped.startswith("!="):
            indent = line[: len(line) - len(stripped)]
            lines[i] = "%s__rustpy_nb__.magic(%r)" % (indent, stripped.rstrip("\r"))
            changed = True
    return "\n".join(lines) if changed else None


def _parse(source, filename):
    try:
        return ast.parse(source, filename, "exec")
    except SyntaxError:
        # Only source that is not valid Python is checked for magics, so strings that merely
        # contain a line starting with "%" or "!" are left alone.
        transformed = _transform_magics(source)
        if transformed is None:
            raise
    # Parsed outside the handler so that an error is not chained to the first one. The rewritten
    # lines are valid calls: an error now is the user's own mistake, at its line in the cell.
    return ast.parse(transformed, filename, "exec")


def _compile(source, filename):
    """``(body, last)`` code objects: the cell's statements and its trailing expression (either may be None)."""
    tree = _parse(source, filename)
    last = None
    if tree.body and isinstance(tree.body[-1], ast.Expr):
        expr = ast.Expression(tree.body.pop().value)
        ast.fix_missing_locations(expr)
        last = compile(expr, filename, "eval")
    body = compile(tree, filename, "exec") if tree.body else None
    return body, last


def _strip_own_frames(te, own_file, seen):
    """Removes this shim's frames from a TracebackException and the exceptions chained to it."""
    if te is None or id(te) in seen:
        return
    seen.add(id(te))
    te.stack = traceback.StackSummary.from_list([f for f in te.stack if f.filename != own_file])
    _strip_own_frames(te.__cause__, own_file, seen)
    _strip_own_frames(te.__context__, own_file, seen)
    for sub in getattr(te, "exceptions", None) or ():
        _strip_own_frames(sub, own_file, seen)


def _report(exc_info, with_frames, source):
    etype, value, tb = exc_info
    try:
        evalue = str(value)
    except Exception:  # noqa: BLE001
        evalue = "<unprintable %s object>" % (etype.__name__,)
    if isinstance(value, UsageError):
        lines = ["UsageError: %s" % (evalue,)]
    else:
        if isinstance(value, SyntaxError) and value.text is None and value.lineno:
            # Errors raised by the compiler (not the parser) carry no source line.
            source_lines = source.splitlines()
            if 1 <= value.lineno <= len(source_lines):
                value.text = source_lines[value.lineno - 1]
        te = traceback.TracebackException(etype, value, tb if with_frames else None)
        _strip_own_frames(te, _report.__code__.co_filename, set())
        lines = "".join(te.format()).rstrip("\n").split("\n")
    _emit({"type": "error", "ename": etype.__name__, "evalue": evalue, "traceback": lines})


def _execute(code, filename, timed=False):
    """Runs cell code: its statements, then its trailing expression, whose value is shown unless
    the code ends with ``;``. ``timed`` prints the time taken (``%%time``)."""
    try:
        body, last = _compile(code, filename)
    except Exception:  # noqa: BLE001 - SyntaxError, ValueError (null bytes), RecursionError …
        _report(sys.exc_info(), False, code)
        return
    try:
        started = (time.perf_counter(), _cpu_times()) if timed else None
        if body is not None:
            exec(body, _USER_NS)  # noqa: F821 - provided by the runner
        value = eval(last, _USER_NS) if last is not None else None  # noqa: F821
        if started is not None:
            _print_times(*started)
        if value is not None and not _quiet(code):
            builtins._ = value
            data, metadata = _bundle(value)
            _emit({"type": "execute_result", "data": data, "metadata": metadata})
    except BaseException:  # noqa: BLE001 - KeyboardInterrupt and SystemExit are cell errors too
        _report(sys.exc_info(), True, code)


def run(count, source):
    """Executes one cell; the runner sends ``__rustpy_nb__.run(count, source)``."""
    del _shown_figures[:]
    filename = "<cell-%d>" % (count,)
    linecache.cache[filename] = (len(source), None, source.splitlines(True), filename)
    try:
        if source.lstrip().startswith("%%"):
            try:
                _cell_magic(source, filename)
            except BaseException:  # noqa: BLE001 - reported like any other cell error
                _report(sys.exc_info(), True, source)
        else:
            _execute(source, filename)
        _flush_figures()
    finally:
        _flush_streams()


class _MainModule(types.ModuleType):
    """``sys.modules["__main__"]`` for the notebook: its attributes are the user namespace."""

    def __getattr__(self, name):
        try:
            return _USER_NS[name]  # noqa: F821
        except KeyError:
            raise AttributeError("module '__main__' has no attribute %r" % (name,)) from None

    def __setattr__(self, name, value):
        _USER_NS[name] = value  # noqa: F821

    def __delattr__(self, name):
        try:
            del _USER_NS[name]  # noqa: F821
        except KeyError:
            raise AttributeError(name) from None

    def __dir__(self):
        return sorted(set(_USER_NS) | set(self.__dict__))  # noqa: F821


# A non-interactive matplotlib backend is used so plt.show() never blocks on a window; its
# "cannot be shown" warning is pointless here because open figures are captured after each cell.
warnings.filterwarnings("ignore", message=".*non-interactive, and thus cannot be shown", category=UserWarning)
if not hasattr(builtins, "display"):
    builtins.display = display
sys.modules["__main__"] = _MainModule("__main__")
_USER_NS["__rustpy_nb__"] = types.SimpleNamespace(run=run, display=display, magic=magic)  # noqa: F821
