"""Preserve the selected native runner while installing explicit failure hooks."""
import os
import runpy
import sys
import rustpy_debug_failures

framework, arguments = sys.argv[1], sys.argv[2:]
rustpy_debug_failures.install(framework)
if not arguments:
    raise ValueError('Missing native test runner')
if arguments[0] == '-m':
    sys.argv[:] = arguments[1:]
    runpy.run_module(arguments[1], run_name='__main__', alter_sys=True)
else:
    sys.argv[:] = arguments
    runpy.run_path(arguments[0], run_name='__main__')
