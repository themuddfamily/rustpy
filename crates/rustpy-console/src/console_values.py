"""Shared console/debugger value protocol. Imported without starting a REPL."""
import ast
import builtins
import collections
import inspect
import itertools
import sys
import types

MAX_PAGE = 200
MAX_REPR = 4000


def safe_repr(value, limit=120):
    limit = max(16, min(int(limit), MAX_REPR))
    try:
        text = repr(value)
    except BaseException as exc:
        text = '<repr failed: %s>' % type(exc).__name__
    text = text.replace('\n', '\\n')
    return text if len(text) <= limit else text[:limit-1] + '…'


def summary(value):
    module = sys.modules.get('pandas')
    if module is not None and isinstance(value, getattr(module, 'DataFrame', ())):
        return 'DataFrame %s × %s' % value.shape
    module = sys.modules.get('numpy')
    if module is not None and isinstance(value, getattr(module, 'ndarray', ())):
        return 'ndarray shape=%s dtype=%s' % (value.shape, value.dtype)
    return safe_repr(value)


class Values:
    def __init__(self, namespace, scientific_table=None):
        self.namespace = namespace
        # The console injects its installed adapter before replacing the helper
        # import path with the project. Keep it outside the user's namespace.
        self.scientific_table = scientific_table
        self.generation = 0
        self.handles = collections.OrderedDict()
        self.sequence = 0

    def changed(self):
        self.generation += 1
        self.handles.clear()

    def retain(self, value, setter=None):
        self.sequence += 1
        token = '@%s:%s' % (self.generation, self.sequence)
        self.handles[token] = (value, setter)
        while len(self.handles) > 1024:
            self.handles.popitem(last=False)
        return token

    def resolve(self, expression):
        if expression.startswith('@'):
            if expression not in self.handles:
                raise ValueError('This inspected value is stale; refresh variables')
            return self.handles[expression][0]
        return eval(expression, self.namespace)

    def info(self, name, value, expression=None, setter=None, descriptor=False):
        if expression is None:
            expression = self.retain(value, setter)
        scalar = isinstance(value, (str, bytes, int, float, complex, bool, type(None)))
        container = isinstance(value, (dict, list, tuple, set, frozenset))
        try:
            has_children = bool(len(value)) if container else not scalar and not callable(value)
        except BaseException:
            has_children = False
        try:
            display = summary(value)
        except BaseException:
            display = safe_repr(value)
        return dict(name=str(name), expr=expression, type_name=type(value).__name__,
                    value='<descriptor; evaluate explicitly>' if descriptor else display,
                    is_callable=callable(value), is_module=isinstance(value, types.ModuleType),
                    has_children=has_children and not descriptor)

    def page(self, expression, start, count):
        start = max(0, int(start)); count = max(1, min(MAX_PAGE, int(count)))
        value = self.resolve(expression)
        if isinstance(value, dict):
            total = len(value)
            pairs = itertools.islice(value.items(), start, start+count)
            items = [self.info(safe_repr(key, 80), child, setter=(value, 'item', key)) for key, child in pairs]
        elif isinstance(value, (list, tuple)):
            total = len(value)
            items = [self.info(str(index), value[index], setter=(value, 'item', index)) for index in range(start, min(total, start+count))]
        elif isinstance(value, (set, frozenset)):
            total = len(value)
            items = [self.info(str(index), child) for index, child in enumerate(itertools.islice(value, start, start+count), start)]
        else:
            names = sorted(set(dir(value)), key=lambda name: (name.startswith('_'), name))
            total = len(names)
            items = []
            for name in names[start:start+count]:
                try:
                    child = inspect.getattr_static(value, name)
                except AttributeError:
                    continue
                descriptor = isinstance(child, property) or (hasattr(type(child), '__get__') and not isinstance(child, (types.FunctionType, staticmethod, classmethod)))
                items.append(self.info(name, child, setter=(value, 'attr', name), descriptor=descriptor))
        return dict(start=start, total=total, items=items)

    def variables(self, start, count, include_modules=False):
        names = sorted(name for name, value in self.namespace.items()
                       if not name.startswith('_') and name != 'runfile'
                       and (include_modules or not isinstance(value, types.ModuleType)))
        start = max(0, int(start)); count = max(1, min(MAX_PAGE, int(count)))
        return dict(start=start, total=len(names), items=[self.info(name, self.namespace[name], name) for name in names[start:start+count]])

    def set_value(self, expression, source):
        value = eval(source, self.namespace)
        if expression.startswith('@'):
            self.resolve(expression)
            setter = self.handles[expression][1]
            if setter is None:
                raise ValueError('This value has no assignable location')
            owner, kind, key = setter
            if kind == 'attr':
                setattr(owner, key, value)
            else:
                owner[key] = value
        else:
            target = ast.parse(expression, mode='eval').body
            if isinstance(target, ast.Name):
                self.namespace[target.id] = value
            elif isinstance(target, ast.Attribute):
                owner = eval(compile(ast.Expression(target.value), '<console-set>', 'eval'), self.namespace)
                setattr(owner, target.attr, value)
            elif isinstance(target, ast.Subscript):
                owner = eval(compile(ast.Expression(target.value), '<console-set>', 'eval'), self.namespace)
                key = eval(compile(ast.Expression(target.slice), '<console-set>', 'eval'), self.namespace)
                owner[key] = value
            else:
                raise ValueError('Select a variable, attribute or item to assign')
        self.changed()

    def data(self, expression, row_start=0, row_count=100, col_start=0, col_count=20, options=None):
        scientific_table = self.scientific_table
        if scientific_table is None:
            from rustpy_scientific_table import scientific_table
        settings = dict(row_start=row_start, row_count=row_count, col_start=col_start, col_count=col_count)
        settings.update(options or {})
        result = scientific_table(self.resolve(expression), settings)
        result['expression'] = expression
        return result

    def dispatch(self, request):
        op = request.get('op'); name = request.get('name') or ''
        response = dict(id=request.get('id'), event=op, name=name, error=None)
        try:
            if op == 'page':
                response.update(self.page(name, request.get('start', 0), request.get('count', 100)))
            elif op == 'vars_page':
                response['event'] = 'page'; response['name'] = '<namespace>'
                response.update(self.variables(request.get('start', 0), request.get('count', 100), request.get('include_modules', False)))
            elif op == 'set':
                self.set_value(name, request.get('value') or '')
            elif op == 'data':
                response['table'] = self.data(name, request.get('row_start', 0), request.get('row_count', 100), request.get('col_start', 0), request.get('col_count', 20), request.get('options'))
            else:
                raise ValueError('Unknown value protocol operation')
        except BaseException as exc:
            response['error'] = '%s: %s' % (type(exc).__name__, safe_repr(exc, 1000))
            response.setdefault('items', []); response.setdefault('total', 0); response.setdefault('start', 0)
        return response


def execute(source, global_namespace, local_namespace=None):
    """Shared console/debugger evaluation; debugpy still owns frame mutation."""
    local_namespace = global_namespace if local_namespace is None else local_namespace
    tree = ast.parse(source, '<console>', 'exec')
    if tree.body and isinstance(tree.body[-1], ast.Expr):
        tail = tree.body.pop()
        if tree.body:
            exec(compile(tree, '<console>', 'exec'), global_namespace, local_namespace)
        expression = ast.Expression(tail.value); ast.fix_missing_locations(expression)
        value = eval(compile(expression, '<console>', 'eval'), global_namespace, local_namespace)
        if value is not None:
            builtins._ = value
        return value
    exec(compile(tree, '<console>', 'exec'), global_namespace, local_namespace)
    return None
