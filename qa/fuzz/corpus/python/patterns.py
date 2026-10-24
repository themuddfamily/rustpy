match subject:
    case {"name": str(name), **rest} if name:
        result = [value async for value in source if value is not None]
