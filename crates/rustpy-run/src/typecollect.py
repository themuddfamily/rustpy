"""RustPy run-time type collector.

Usage: python typecollect.py OUT.json PROJECT_ROOT (script.py | -m module) [args...]

Records the types of arguments and return values of functions defined under
PROJECT_ROOT (virtualenvs and site-packages excluded) and merges them into
OUT.json as {"version": 1, "functions": {"<file>::<qualname>": {"params":
{name: [types]}, "return": [types]}}}.
"""
import atexit
import json
import os
import runpy
import sys
import threading

MAX_TYPES = 8


def main():
    if len(sys.argv) < 4:
        sys.exit("typecollect: usage: typecollect.py OUT.json PROJECT_ROOT script|-m module [args]")
    out_path = sys.argv[1]
    root = os.path.normcase(os.path.realpath(sys.argv[2]))
    rest = sys.argv[3:]
    data = {}
    cache = {}

    def tname(value):
        t = type(value)
        mod = getattr(t, "__module__", "builtins")
        q = getattr(t, "__qualname__", t.__name__)
        return q if mod == "builtins" else mod + "." + q

    def in_project(filename):
        hit = cache.get(filename)
        if hit is None and (filename.startswith("<") or not os.path.isfile(filename)):
            hit = cache[filename] = False  # frozen modules, exec() strings
        if hit is None:
            try:
                p = os.path.normcase(os.path.realpath(filename))
            except Exception:
                p = ""
            sep = os.sep
            hit = p.startswith(root + sep) and (sep + "site-packages" + sep) not in p and (sep + ".venv" + sep) not in p and (sep + "venv" + sep) not in p
            cache[filename] = hit
        return hit

    def add(lst, t):
        if t not in lst and len(lst) < MAX_TYPES:
            lst.append(t)

    def profiler(frame, event, arg):
        if event != "call" and event != "return":
            return
        code = frame.f_code
        if code.co_name.startswith("<") or not in_project(code.co_filename):
            return
        key = os.path.realpath(code.co_filename) + "::" + getattr(code, "co_qualname", code.co_name)
        entry = data.get(key)
        if entry is None:
            entry = data[key] = {"params": {}, "return": []}
        if event == "call":
            n = code.co_argcount + code.co_kwonlyargcount
            local_vars = frame.f_locals
            for name in code.co_varnames[:n]:
                if name in ("self", "cls") or name not in local_vars:
                    continue
                add(entry["params"].setdefault(name, []), tname(local_vars[name]))
        else:
            add(entry["return"], tname(arg))

    def dump():
        sys.setprofile(None)
        threading.setprofile(None)
        try:
            with open(out_path, encoding="utf-8") as f:
                merged = json.load(f).get("functions", {})
        except Exception:
            merged = {}
        for key, entry in data.items():
            old = merged.setdefault(key, {"params": {}, "return": []})
            for name, types in entry["params"].items():
                lst = old.setdefault("params", {}).setdefault(name, [])
                for t in types:
                    add(lst, t)
            for t in entry["return"]:
                add(old.setdefault("return", []), t)
        try:
            os.makedirs(os.path.dirname(out_path), exist_ok=True)
            tmp = out_path + ".tmp"
            with open(tmp, "w", encoding="utf-8") as f:
                json.dump({"version": 1, "functions": merged}, f, indent=1, sort_keys=True)
            os.replace(tmp, out_path)
        except Exception as e:  # never break the user's program on exit
            sys.stderr.write("typecollect: cannot write %s: %s\n" % (out_path, e))

    atexit.register(dump)
    if rest[0] == "-m":
        if len(rest) < 2:
            sys.exit("typecollect: -m needs a module name")
        sys.argv[:] = rest[1:]
        sys.path[0] = os.getcwd()
        threading.setprofile(profiler)
        sys.setprofile(profiler)
        runpy.run_module(rest[1], run_name="__main__", alter_sys=True)
    else:
        sys.argv[:] = rest
        sys.path[0] = os.path.dirname(os.path.abspath(rest[0]))
        threading.setprofile(profiler)
        sys.setprofile(profiler)
        runpy.run_path(rest[0], run_name="__main__")


main()
