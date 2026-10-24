"""Optional profiling hook, independent of debugpy's tracing/monitoring engine."""
def install(root, output):
    import atexit, json, os, sys, threading
    root = os.path.normcase(os.path.realpath(root)) + os.sep
    data, cache = {}, {}
    previous = sys.getprofile()
    previous_thread = threading.getprofile() if hasattr(threading, 'getprofile') else None
    def typename(value):
        cls = type(value)
        module = getattr(cls, '__module__', 'builtins')
        name = getattr(cls, '__qualname__', cls.__name__)
        return name if module == 'builtins' else module + '.' + name
    def add(values, value):
        if value not in values and len(values) < 8:
            values.append(value)
    def profile(frame, event, result):
        if callable(previous):
            previous(frame, event, result)
        if event not in ('call', 'return'):
            return
        code = frame.f_code
        filename = code.co_filename
        if filename not in cache:
            if len(cache) >= 8192:
                cache.clear()
            path = os.path.normcase(os.path.realpath(filename))
            cache[filename] = path.startswith(root) and not any(os.sep + part + os.sep in path for part in ('site-packages', '.venv', 'venv'))
        if not cache[filename] or code.co_name.startswith('<'):
            return
        key = os.path.realpath(filename) + '::' + getattr(code, 'co_qualname', code.co_name)
        if key not in data and len(data) >= 10000:
            return
        entry = data.setdefault(key, {'params': {}, 'return': []})
        if event == 'return':
            add(entry['return'], typename(result))
        else:
            for name in code.co_varnames[:code.co_argcount + code.co_kwonlyargcount]:
                if name not in ('self', 'cls') and name in frame.f_locals:
                    add(entry['params'].setdefault(name, []), typename(frame.f_locals[name]))
    def finish():
        sys.setprofile(previous)
        threading.setprofile(previous_thread)
        try:
            try:
                with open(output, encoding='utf-8') as stream:
                    merged = json.load(stream).get('functions', {})
            except (OSError, ValueError):
                merged = {}
            for key, entry in data.items():
                target = merged.setdefault(key, {'params': {}, 'return': []})
                for name, types in entry['params'].items():
                    for value in types:
                        add(target['params'].setdefault(name, []), value)
                for value in entry['return']:
                    add(target['return'], value)
            os.makedirs(os.path.dirname(output), exist_ok=True)
            temporary = output + '.' + str(os.getpid()) + '.tmp'
            with open(temporary, 'w', encoding='utf-8') as stream:
                json.dump({'version': 1, 'functions': merged}, stream)
            os.replace(temporary, output)
        except Exception as error:
            sys.stderr.write('RustPy runtime types: ' + str(error) + '\n')
    atexit.register(finish)
    threading.setprofile(profile)
    sys.setprofile(profile)
