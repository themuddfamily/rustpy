"""Interpreter-compatible dispatcher shared by Run/Debug/Coverage/Profile."""
import os
import runpy
import sys

if __name__ == '__main__':
    arguments = sys.argv[1:]
    framework = None
    if '--framework' in arguments:
        position = arguments.index('--framework')
        if position + 1 < len(arguments):
            framework = arguments[position + 1]
    filename = 'rustpy_lettuce_reporter.py' if framework == 'lettuce' else 'rustpy_test_reporter.py'
    script = os.path.join(os.path.dirname(os.path.abspath(__file__)), filename)
    sys.argv[0] = script
    runpy.run_path(script, run_name='__main__')
