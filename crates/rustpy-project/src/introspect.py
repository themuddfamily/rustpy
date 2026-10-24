"""Embedded target-interpreter script: explicit trusted extension admission only."""
import ast, hashlib, importlib.util, inspect, json, keyword, os, pathlib, sys, sysconfig, types, stat

MAX_BINARY = 256 * 1024 * 1024
MAX_MEMBERS = 2048
MAX_PYI = 2 * 1024 * 1024

def digest(path):
    def signature(info):
        return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns)
    before = path.stat(follow_symlinks=False)
    if not stat.S_ISREG(before.st_mode) or before.st_size > MAX_BINARY:
        raise ValueError('nonregular binary or size limit')
    descriptor = os.open(path, os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0))
    with os.fdopen(descriptor, 'rb') as stream:
        if signature(os.fstat(stream.fileno())) != signature(before):
            raise ValueError('binary descriptor changed')
        value = hashlib.file_digest(stream, 'sha256').hexdigest()
        if signature(os.fstat(stream.fileno())) != signature(before):
            raise ValueError('binary changed while hashing')
    if signature(path.stat(follow_symlinks=False)) != signature(before):
        raise ValueError('binary path changed while hashing')
    return value

def name_ok(name):
    return isinstance(name, str) and name.isidentifier() and not keyword.iskeyword(name)

def main():
    request = json.loads(sys.argv[1])
    if request['explicitly_trusted'] is not True:
        raise ValueError('explicit trust required')
    name = request['module_name']
    if not all(name_ok(part) for part in name.split('.')):
        raise ValueError('qualified Python module identifier required')
    path = pathlib.Path(request['module_path']).resolve(strict=True)
    root = pathlib.Path(request['library_root']).resolve(strict=True)
    interpreter = pathlib.Path(sys.executable).resolve(strict=True)
    if not path.is_file() or root not in path.parents:
        raise ValueError('module outside admitted canonical library root')
    if path.suffix.lower() not in ('.so', '.pyd'):
        raise ValueError('only native extension files admitted')
    if not os.path.samefile(interpreter, request['interpreter']):
        raise ValueError('interpreter path identity mismatch')
    identity = dict(interpreter=request['interpreter'], interpreter_sha256=digest(interpreter),
                    version='.'.join(map(str, sys.version_info[:3])),
                    cache_tag=sys.implementation.cache_tag, soabi=sysconfig.get_config_var('SOABI') or '')
    for actual, expected in [('interpreter', 'interpreter'), ('interpreter_sha256', 'interpreter_sha256'),
                             ('version', 'expected_version'), ('cache_tag', 'expected_cache_tag'), ('soabi', 'expected_soabi')]:
        if identity[actual] != request[expected]:
            raise ValueError('interpreter binding mismatch: ' + actual)
    components = name.split('.')
    if path.parent != root.joinpath(*components[:-1]) or not path.name.startswith(components[-1] + '.'):
        raise ValueError('qualified module path does not match approved library root')
    before = digest(path)
    if before != request['module_sha256']:
        raise ValueError('module content mismatch')
    sys.dont_write_bytecode = True
    # Explicit package shells avoid importing package __init__ or project code.
    for index in range(1, len(components)):
        parent_name = '.'.join(components[:index])
        parent = types.ModuleType(parent_name)
        parent.__path__ = [str(root.joinpath(*components[:index]))]
        parent.__package__ = parent_name
        sys.modules[parent_name] = parent
    spec = importlib.util.spec_from_file_location(name, str(path))
    if spec is None or spec.loader is None:
        raise ValueError('extension loader unavailable')
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    unsupported = []
    lines = ['from typing import Any', '']
    count = 0

    def signature(obj, method=False):
        try:
            parameters = list(inspect.signature(obj, eval_str=False).parameters.values())
        except (TypeError, ValueError):
            unsupported.append('signature unavailable: ' + str(getattr(obj, '__name__', '?')))
            return ((('cls' if method == 'cls' else 'self') + ': Any, ') if method else '') + '*args: Any, **kwargs: Any'
        if len(parameters) > 128:
            raise ValueError('signature parameter limit')
        parts = []
        positional = [p for p in parameters if p.kind == p.POSITIONAL_ONLY]
        saw_star = False
        for index, parameter in enumerate(parameters):
            if not name_ok(parameter.name):
                raise ValueError('invalid reflected parameter name')
            if parameter.kind == parameter.KEYWORD_ONLY and not saw_star:
                parts.append('*')
                saw_star = True
            prefix = '*' if parameter.kind == parameter.VAR_POSITIONAL else '**' if parameter.kind == parameter.VAR_KEYWORD else ''
            if parameter.kind == parameter.VAR_POSITIONAL:
                saw_star = True
            value = prefix + parameter.name + ': Any'
            if parameter.default is not parameter.empty:
                # Never call arbitrary default-object repr or evaluate annotations.
                value += ' = ...'
            parts.append(value)
            if positional and index + 1 == len(positional):
                parts.append('/')
        return ', '.join(parts)

    doc_bytes = 0
    def doc_literal(value, mapping=False):
        nonlocal doc_bytes
        try:
            doc = vars(value).get('__doc__') if mapping else value.__doc__
        except Exception:
            return None
        if not isinstance(doc, str) or not doc.strip():
            return None
        doc = inspect.cleandoc(doc)[:8192]
        rendered = repr(doc)
        size = len(rendered.encode('utf-8'))
        if doc_bytes + size > 512 * 1024:
            return None
        doc_bytes += size
        return rendered

    module_doc = doc_literal(module, True)
    if module_doc:
        lines.insert(0, module_doc)

    def members(mapping, indent='', method=False):
        nonlocal count
        if len(mapping) > MAX_MEMBERS:
            raise ValueError("member mapping limit")
        for key, value in sorted(mapping.items()):
            if not name_ok(key):
                unsupported.append('invalid Python member identifier skipped: ' + str(key))
                continue
            if key.startswith('__') and key not in ('__init__', '__new__', '__call__'):
                continue
            count += 1
            if count > MAX_MEMBERS:
                raise ValueError('member limit')
            is_method = method
            if isinstance(value, staticmethod):
                is_method = False
                lines.append(indent + '@staticmethod')
                value = value.__func__
            elif isinstance(value, classmethod):
                is_method = 'cls'
                lines.append(indent + '@classmethod')
                value = value.__func__
            elif isinstance(value, types.ClassMethodDescriptorType):
                is_method = 'cls'
                lines.append(indent + '@classmethod')
            if isinstance(value, (types.ClassMethodDescriptorType, types.BuiltinFunctionType, types.FunctionType, types.MethodDescriptorType, types.WrapperDescriptorType)):
                header = indent + 'def ' + key + '(' + signature(value, is_method) + ') -> Any:'
                documentation = doc_literal(value)
                if documentation:
                    lines.extend((header, indent + '    ' + documentation, indent + '    ...'))
                else:
                    lines.append(header + ' ...')
            elif isinstance(value, type) and not method:
                if value.__module__ != name:
                    unsupported.append('imported class skipped: ' + key)
                    continue
                lines.append('class ' + key + ':')
                start = len(lines)
                documentation = doc_literal(value, True)
                if documentation:
                    lines.append('    ' + documentation)
                members(vars(value), '    ', True)
                if len(lines) == start:
                    lines.append('    ...')
            else:
                if isinstance(value, (types.GetSetDescriptorType, types.MemberDescriptorType)):
                    unsupported.append('descriptor type unavailable: ' + key)
                lines.append(indent + key + ': Any')
    members(vars(module))
    pyi = '\n'.join(lines) + '\n'
    if len(pyi.encode('utf-8')) > MAX_PYI or len(unsupported) > MAX_MEMBERS:
        raise ValueError('stub output limit')
    ast.parse(pyi, filename=name + '.pyi')
    if digest(path) != before or digest(interpreter) != identity['interpreter_sha256']:
        raise ValueError('binary changed during introspection')
    print(json.dumps(dict(protocol=1, module_name=name, module_path=request['module_path'], module_sha256=before,
                         library_root=request['library_root'], **identity, pyi=pyi, unsupported=unsupported), ensure_ascii=True))

if __name__ == '__main__':
    main()
