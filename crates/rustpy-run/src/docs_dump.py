"""Docstrings of builtins and standard-library modules for RustPy's Quick
Documentation: typeshed stubs carry none, and compiled modules (builtins,
math, time, itertools, posix, _io, ...) have no Python source to read them
from. Writes {"format": 1, "version": ..., "docs": {"module.qualname": doc}}
as ASCII JSON to the file named by the first argument.

Run in isolated mode (-I): nothing from the project directory is imported.
"""
import importlib
import inspect
import json
import os
import sys
import warnings

warnings.simplefilter("ignore")

# Public modules (re-exporting compiled implementations) besides every
# compiled-in and extension module of the standard library.
PUBLIC = [
    "abc", "argparse", "array", "asyncio", "atexit", "base64", "binascii", "bisect", "builtins",
    "bz2", "calendar", "cmath", "codecs", "collections", "collections.abc", "contextlib",
    "contextvars", "copy", "csv", "ctypes", "dataclasses", "datetime", "decimal", "difflib",
    "enum", "errno", "faulthandler", "fcntl", "fnmatch", "fractions", "functools", "gc", "glob",
    "grp", "gzip", "hashlib", "heapq", "hmac", "html", "inspect", "io", "itertools", "json",
    "locale", "logging", "lzma", "marshal", "math", "mmap", "operator", "os", "os.path",
    "pathlib", "pickle", "platform", "pprint", "pwd", "queue", "random", "re", "resource",
    "secrets", "select", "selectors", "shutil", "signal", "socket", "sqlite3", "ssl", "stat",
    "statistics", "string", "struct", "subprocess", "sys", "tempfile", "termios", "textwrap",
    "threading", "time", "traceback", "types", "typing", "unicodedata", "unittest",
    "urllib.parse", "urllib.request", "uuid", "warnings", "weakref", "zlib", "zoneinfo",
]
# Never imported: side effects (antigravity opens a browser, this prints),
# GUI toolkits, terminal state, CPython's test helpers.
SKIP = ("antigravity", "this", "idlelib", "tkinter", "_tkinter", "turtle", "turtledemo",
        "readline", "_curses_panel", "test", "_test", "xx", "_xx", "_ctypes_test", "__")
MAX_DOC = 6000
# Members documented with the class itself rather than one by one.
SKIP_MEMBERS = {"__doc__", "__module__", "__dict__", "__weakref__", "__slots__",
                "__annotations__", "__qualname__", "__name__", "__init_subclass__",
                "__subclasshook__", "__class_getitem__"}


def compiled_modules():
    names = set(sys.builtin_module_names)
    dirs = []
    try:
        import sysconfig
        dirs.append(os.path.join(sysconfig.get_path("platstdlib"), "lib-dynload"))
    except Exception:
        pass
    dirs.append(os.path.join(getattr(sys, "base_prefix", sys.prefix), "DLLs"))
    try:
        from importlib.machinery import EXTENSION_SUFFIXES
    except Exception:
        EXTENSION_SUFFIXES = []
    for d in dirs:
        try:
            entries = os.listdir(d)
        except OSError:
            continue
        for f in entries:
            for suffix in EXTENSION_SUFFIXES:
                if f.endswith(suffix):
                    names.add(f[: -len(suffix)])
                    break
    return names


def clean(doc):
    if not isinstance(doc, str):
        return None
    doc = inspect.cleandoc(doc).strip()
    if not doc:
        return None
    return doc[:MAX_DOC]


def own_doc(obj):
    # Classes and modules: their own docstring only (inspect.getdoc would show
    # a base class's for a class without one).
    try:
        return clean(obj.__doc__)
    except Exception:
        return None


def routine_doc(obj):
    try:
        return clean(inspect.getdoc(obj))
    except Exception:
        return None


def is_member(value):
    return (inspect.isroutine(value) or inspect.isdatadescriptor(value)
            or isinstance(value, (staticmethod, classmethod, property)))


def dump_class(prefix, cls, out):
    try:
        members = list(vars(cls).items())
    except Exception:
        return
    for name, value in members:
        if name in SKIP_MEMBERS or (name.startswith("__") and name.endswith("__")) or not is_member(value):
            continue
        if isinstance(value, property):
            doc = routine_doc(value)
        else:
            try:
                doc = routine_doc(getattr(cls, name))
            except Exception:
                doc = routine_doc(value)
        if doc:
            out[prefix + "." + name] = doc


def dump_module(name, out):
    try:
        module = importlib.import_module(name)
    except BaseException:
        return
    doc = own_doc(module)
    if doc:
        out[name] = doc
    for attr in dir(module):
        if attr.startswith("_") and not (name == "builtins" and attr in ("__import__", "__build_class__")):
            continue
        try:
            obj = getattr(module, attr)
        except BaseException:
            continue
        key = name + "." + attr
        if inspect.isclass(obj):
            doc = own_doc(obj)
            if doc:
                out[key] = doc
            dump_class(key, obj, out)
        elif inspect.isroutine(obj):
            doc = routine_doc(obj)
            if doc:
                out[key] = doc


def main():
    out = {}
    names = set(PUBLIC) | compiled_modules()
    for name in sorted(names):
        if name.startswith(SKIP):
            continue
        dump_module(name, out)
    data = {"format": 1, "version": "%d.%d.%d" % sys.version_info[:3],
            "implementation": sys.implementation.name, "docs": out}
    with open(sys.argv[1], "w", encoding="ascii") as f:
        json.dump(data, f, ensure_ascii=True, sort_keys=True)


main()
