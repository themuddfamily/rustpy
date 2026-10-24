"""Installable Agg/HTML capture without importing scientific libraries eagerly."""
import sys as _rp_sys

def scientific_install(enabled=True):
    import os, builtins
    state = getattr(builtins, '_rustpy_scientific', None)
    if state is None:
        state = dict(enabled=bool(enabled), plots=[], seen={}, patched=set())
        builtins._rustpy_scientific = state
    state['enabled'] = bool(enabled)
    if enabled:
        os.environ['MPLBACKEND'] = 'Agg'
        if 'import_hook' not in state:
            original_import = builtins.__import__
            def import_hook(name, *args, **kwargs):
                module = original_import(name, *args, **kwargs)
                if name.startswith(('matplotlib', 'plotly', 'bokeh')) and not state.get('patching'):
                    state['patching'] = True
                    try:
                        scientific_patch_interactive()
                    finally:
                        state['patching'] = False
                return module
            builtins.__import__ = import_hook
            state['import_hook'] = original_import
    return state

def scientific_capture():
    import io, base64, builtins
    state = scientific_install(getattr(builtins, '_rustpy_scientific', {}).get('enabled', True))
    if not state['enabled']:
        return []
    pyplot = _rp_sys.modules.get('matplotlib.pyplot')
    if pyplot is not None:
        visible_numbers = pyplot.get_fignums()[-64:]
        state['seen'] = {number:digest for number,digest in state['seen'].items() if number in visible_numbers}
        for number in visible_numbers:
            figure = pyplot.figure(number)
            if figure.get_figwidth()*100*figure.get_figheight()*100 > 8*1024*1024:
                raise ValueError("Figure exceeds the 8 megapixel capture budget")
            stream = io.BytesIO()
            figure.savefig(stream, format='png', dpi=100)
            image = stream.getvalue()
            if len(image) <= 8 * 1024 * 1024:
                import hashlib
                digest = hashlib.sha256(image).hexdigest()
                if state['seen'].get(number) != digest:
                    state['seen'][number] = digest
                    state['plots'].append(dict(mime='image/png', data_base64=base64.b64encode(image).decode('ascii'), title='Figure %s' % number))
                    while sum(len(plot['data_base64']) for plot in state['plots']) > 64*1024*1024:
                        state['plots'].pop(0)
    result, state['plots'] = state['plots'][-64:], []
    output = __import__('os').environ.get('RUSTPY_SCIENTIFIC_OUTPUT')
    if output and result:
        import json
        import os
        if os.path.exists(output) and os.path.getsize(output) > 128 * 1024 * 1024:
            raise ValueError('Scientific capture log reached its 128 MiB session budget')
        with open(output, 'a', encoding='utf-8') as stream:
            for plot in result:
                stream.write(json.dumps(plot) + '\n')
        return []
    return result

def scientific_patch_interactive():
    """Called after each command; wrappers also capture subsequent explicit show()."""
    import builtins, base64
    state = scientific_install(getattr(builtins, '_rustpy_scientific', {}).get('enabled', True))
    if not state['enabled']:
        return
    def retain(html, title):
        data = html.encode('utf-8')
        if len(data) > 16 * 1024 * 1024:
            raise ValueError('Interactive plot exceeds 16 MiB')
        state['plots'].append(dict(mime='text/html', data_base64=base64.b64encode(data).decode('ascii'), title=title))
        state['plots'][:] = state['plots'][-64:]
        while sum(len(plot['data_base64']) for plot in state['plots']) > 64*1024*1024:
            state['plots'].pop(0)
        if __import__('os').environ.get('RUSTPY_SCIENTIFIC_OUTPUT'):
            scientific_capture()
    pyplot = _rp_sys.modules.get('matplotlib.pyplot')
    if pyplot is not None and hasattr(pyplot, 'show') and 'matplotlib' not in state['patched']:
        original_show = pyplot.show
        def mpl_show(*args, **kwargs):
            if state['enabled']:
                captured = scientific_capture()
                state['plots'].extend(captured)
            else:
                return original_show(*args, **kwargs)
        pyplot.show = mpl_show
        state['patched'].add('matplotlib')
    if __import__('os').environ.get('RUSTPY_SCIENTIFIC_INTERACTIVE', '1') == '0':
        return
    plotly = _rp_sys.modules.get('plotly.basedatatypes')
    if plotly is not None and hasattr(plotly, 'BaseFigure') and 'plotly' not in state['patched']:
        original = plotly.BaseFigure.show
        def show(figure, *args, **kwargs):
            if state['enabled']:
                retain(figure.to_html(full_html=True, include_plotlyjs=True), 'Plotly')
            else:
                return original(figure, *args, **kwargs)
        plotly.BaseFigure.show = show
        state['patched'].add('plotly')
    bokeh = _rp_sys.modules.get('bokeh.io.showing')
    if bokeh is not None and hasattr(bokeh, 'show') and 'bokeh' not in state['patched']:
        original = bokeh.show
        def show(obj, *args, **kwargs):
            if state['enabled']:
                from bokeh.embed import file_html
                from bokeh.resources import INLINE
                retain(file_html(obj, INLINE, 'Bokeh plot'), 'Bokeh')
            else:
                return original(obj, *args, **kwargs)
        bokeh.show = show
        io_module = _rp_sys.modules.get('bokeh.io')
        if io_module is not None:
            io_module.show = show
        plotting = _rp_sys.modules.get('bokeh.plotting')
        if plotting is not None:
            plotting.show = show
        state['patched'].add('bokeh')
