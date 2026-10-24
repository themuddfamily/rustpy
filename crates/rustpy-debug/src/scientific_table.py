"""Shared bounded scientific adapters; imported by consoles and evaluated by DAP.
No libraries are imported merely to inspect unrelated values.
"""
def scientific_table(value, options=None):
    import sys, math, itertools
    options = options or {}
    original_type = type(value).__module__ + '.' + type(value).__name__
    shape = list(getattr(value, 'shape', ()))
    dtype = str(getattr(value, 'dtype', ''))
    row_start = max(0, int(options.get('offset', options.get('row_start', 0))))
    col_start = max(0, int(options.get('col_start', 0)))
    width = max(1, min(200, int(options.get('columns', options.get('col_count', 50)))))
    limit = max(1, min(1000, int(options.get('rows', options.get('row_count', 100))), 10000 // width))
    selection = str(options.get('slice', '')).strip()
    def parse_axes(selection):
        axes = []
        for axis in selection.split(','):
            axis = axis.strip()
            if ':' in axis:
                parts = [int(part) if part.strip() else None for part in axis.split(':')]
                if len(parts) > 3 or (len(parts) == 3 and parts[2] == 0):
                    raise ValueError('Slice accepts start:stop:step with a nonzero step')
                axes.append(slice(*parts))
            else:
                axes.append(int(axis))
        return axes
    if selection and hasattr(value, 'iloc'):
        axes = parse_axes(selection)
        value = value.iloc[tuple(axes)] if len(axes) > 1 else value.iloc[axes[0]]
        if not hasattr(value, 'iloc'):
            value = [[value]]
    if selection and original_type.startswith('polars.'):
        axes = parse_axes(selection)
        value = value[axes[0], axes[1] if len(axes) > 1 else slice(None)] if type(value).__name__ == 'DataFrame' else value[axes[0]]
    if original_type.startswith('polars.') and not hasattr(value, 'shape'):
        value = [[value]]
    column_page = None
    if original_type.startswith('polars.') and hasattr(value, 'shape'):
        # Polars to_numpy would require numpy and materialize the whole table.
        if type(value).__name__ == 'Series':
            value = value.to_frame()
        columns = list(value.columns)
        nr, nc = value.shape
        get_cell = lambda r, c: value.item(r, c)
        get_index = str
    elif hasattr(value, 'iloc'):
        if not hasattr(value, 'columns'):
            value = value.to_frame()
        columns = value.columns
        nr, nc = value.shape
        get_cell = lambda r, c: value.iloc[r, c]
        get_index = lambda r: str(value.index[r])[:256]
    elif hasattr(value, 'shape'):
        if selection:
            value = value[tuple(parse_axes(selection))]
        ndim = len(value.shape)
        if ndim > 2:
            raise ValueError('Choose an explicit slice reducing the tensor to at most two dimensions')
        nr, nc = (1, 1) if ndim == 0 else ((int(value.shape[0]), 1) if ndim == 1 else map(int, value.shape))
        columns = range(nc)
        def get_cell(r, c):
            item = value if ndim == 0 else (value[r] if ndim == 1 else value[r, c])
            # Transfer only requested scalar cells from accelerators. Never
            # materialize the complete tensor merely to display a small page.
            if original_type.startswith('torch.'):
                return item.detach().cpu().item()
            if original_type.startswith('tensorflow.'):
                return item.numpy().item()
            if hasattr(item, 'item'):
                return item.item()
            return item
        get_index = str
    elif isinstance(value, dict):
        nr, nc = len(value), 2
        columns = ['key', 'value']
        keys = list(itertools.islice(value, row_start, min(nr, row_start+limit)))
        get_cell = lambda r, c: keys[r-row_start] if c == 0 else value[keys[r-row_start]]
        get_index = str
    elif isinstance(value, (list, tuple)):
        nr = len(value)
        first = value[0] if value else None
        keys = list(itertools.islice(first, col_start, col_start+width)) if isinstance(first, dict) else None
        nc = len(first) if keys is not None else (len(first) if isinstance(first, (list, tuple)) else 1)
        columns = range(nc)
        if keys is not None:
            column_page = [str(key) for key in keys]
        def get_cell(r, c):
            row = value[r]
            if keys is not None:
                return row.get(keys[c-col_start]) if isinstance(row, dict) else None
            if isinstance(row, (list, tuple)):
                return row[c] if c < len(row) else None
            return row
        get_index = str
    else:
        raise TypeError('Expected a DataFrame, Series, ndarray, tensor, Polars table or collection')
    selected_columns = column_page if column_page is not None else [str(column)[:256] for column in columns[col_start:col_start+width]]
    stop = min(nr, row_start+limit)
    raw = [[get_cell(r, c) for c in range(col_start, min(nc, col_start+width))] for r in range(row_start, stop)]
    index = [get_index(r) for r in range(row_start, stop)]
    text_budget = [512 * 1024]
    def text(item):
        try:
            result = str(item)[:min(256, text_budget[0])]
            text_budget[0] = max(0, text_budget[0] - len(result.encode("utf-8", "replace")))
            return result
        except BaseException:
            return '<unprintable>'
    rows = [[text(item) for item in row] for row in raw]
    stats = []
    for c, name in enumerate(selected_columns):
        numbers, missing = [], 0
        for row in raw:
            item = row[c]
            pd = sys.modules.get('pandas')
            if item is None or (pd is not None and (item is getattr(pd, 'NA', object()) or item is getattr(pd, 'NaT', object()))):
                missing += 1
                continue
            try:
                number = float(item)
                if math.isfinite(number):
                    numbers.append(number)
                else:
                    missing += 1
            except (TypeError, ValueError, OverflowError):
                pass
        count = len(numbers)
        mean = sum(number/count for number in numbers) if count else None
        deviation = math.hypot(*(number-mean for number in numbers))/math.sqrt(count) if count else None
        mean = mean if mean is None or math.isfinite(mean) else None
        deviation = deviation if deviation is None or math.isfinite(deviation) else None
        stats.append(dict(column=name, count=len(raw), numeric=count, missing=missing,
                          minimum=min(numbers) if count else None, maximum=max(numbers) if count else None,
                          mean=mean, stddev=deviation,
                          scope='loaded page'))
    return dict(type=original_type, kind=original_type, dtype=dtype, shape=shape,
                columns=selected_columns, rows=rows, index=index, stats=stats,
                offset=row_start, total=int(nr), row_start=row_start, col_start=col_start,
                total_rows=int(nr), total_cols=int(nc), truncated=text_budget[0] == 0 or row_start > 0 or col_start > 0 or stop < nr or col_start+len(selected_columns) < nc)
