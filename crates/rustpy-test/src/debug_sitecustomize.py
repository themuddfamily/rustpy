"""Explicit opt-in for Python commands inside a tox test environment."""
import os
import sys
if os.environ.get('RUSTPY_DEBUG_TEST_AUTOINSTALL') == '1':
    import rustpy_debug_failures
    rustpy_debug_failures.install('child')
    rustpy_debug_failures.install_import_hooks()

# Preserve an existing environment/project sitecustomize after installing the
# opted-in hooks, without importing this same sidecar recursively.
import importlib.machinery
import importlib.util
_directory = os.path.realpath(os.path.dirname(__file__))
_paths = [path for path in sys.path if os.path.realpath(path or os.getcwd()) != _directory]
_spec = importlib.machinery.PathFinder.find_spec('sitecustomize', _paths)
if _spec is not None and _spec.loader is not None:
    _module = importlib.util.module_from_spec(_spec)
    sys.modules['sitecustomize'] = _module
    _spec.loader.exec_module(_module)
