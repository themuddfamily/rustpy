"""Bounded explicit value renderers evaluated inside the selected debug frame."""
def render(value, kind, options):
    import json
    import inspect
    limit = min(max(int(options.get('rows', 100)), 1), 1000)
    offset = max(int(options.get('offset', 0)), 0)
    if kind == 'json':
        chunks, size = [], 0
        for chunk in json.JSONEncoder(default=repr, ensure_ascii=False).iterencode(value):
            size += len(chunk)
            if size > 1048576:
                raise ValueError('JSON exceeds debugger preview budget')
            chunks.append(chunk)
        return ''.join(chunks)
    if kind == 'text':
        return str(value)[:1048576]
    if kind == 'bytes':
        return bytes(value)[:65536].hex(' ')
    if kind == 'html':
        return str(value._repr_html_() if hasattr(value, '_repr_html_') else value)[:1048576]
    if kind in ('source', 'type_source'):
        target = type(value) if kind == 'type_source' else value
        try:
            filename = inspect.getsourcefile(target) or inspect.getfile(target)
            line = inspect.getsourcelines(target)[1]
            return json.dumps({'path': filename, 'line': line})
        except (TypeError, OSError):
            return json.dumps({'error': 'No Python source available for this object'})
    if kind == 'referrers':
        import gc
        return '\n'.join(type(obj).__name__ + ': ' + repr(obj)[:300] for obj in gc.get_referrers(value)[:50])
    if kind == 'image':
        import io, base64
        class PreviewBuffer(io.BytesIO):
            def write(self, chunk):
                if self.tell() + len(chunk) > 4 * 1024 * 1024:
                    raise ValueError('Image exceeds debugger preview budget')
                return super().write(chunk)
        stream = PreviewBuffer()
        value.save(stream, format='PNG')
        if stream.tell() > 4 * 1024 * 1024:
            raise ValueError('Image exceeds debugger preview budget')
        return base64.b64encode(stream.getvalue()).decode('ascii')
    if kind == 'table':
        return json.dumps(scientific_table(value, options), ensure_ascii=False)
    if kind == 'plots_disabled':
        scientific_install(False)
        return '[]'
    if kind == 'plots':
        scientific_install(True)
        scientific_patch_interactive()
        return json.dumps(scientific_capture())
    raise ValueError('Unknown debugger renderer: ' + kind)

def async_snapshot():
    import asyncio, json
    try:
        tasks = list(asyncio.all_tasks())[:256]
    except RuntimeError:
        return 'No running asyncio event loop in this suspended thread'
    result = []
    for task in tasks:
        chain, current, visited = [], task.get_coro(), set()
        while current is not None and id(current) not in visited and len(chain) < 64:
            visited.add(id(current))
            frame = getattr(current, 'cr_frame', None) or getattr(current, 'gi_frame', None)
            if frame is not None:
                chain.append({'name': frame.f_code.co_name, 'path': frame.f_code.co_filename, 'line': frame.f_lineno})
            current = getattr(current, 'cr_await', None) or getattr(current, 'gi_yieldfrom', None)
        result.append({'name': task.get_name(), 'done': task.done(), 'cancelled': task.cancelled(), 'await_chain': chain})
    return json.dumps(result, ensure_ascii=False)
