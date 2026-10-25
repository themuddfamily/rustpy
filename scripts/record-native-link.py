#!/usr/bin/env python3
"""Run the real linker and retain the command, response files and output hashes."""
import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys


def digest(path):
    with Path(path).open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def response_arguments(text):
    if os.name != 'nt':
        return shlex.split(text)
    shell = ctypes.windll.shell32
    shell.CommandLineToArgvW.argtypes = [ctypes.c_wchar_p, ctypes.POINTER(ctypes.c_int)]
    shell.CommandLineToArgvW.restype = ctypes.POINTER(ctypes.c_wchar_p)
    count = ctypes.c_int()
    arguments = shell.CommandLineToArgvW('link.exe ' + text, ctypes.byref(count))
    if not arguments:
        raise OSError('Cannot parse linker response file')
    try:
        return [arguments[index] for index in range(1, count.value)]
    finally:
        ctypes.windll.kernel32.LocalFree.argtypes = [ctypes.c_void_p]
        ctypes.windll.kernel32.LocalFree(arguments)


def retain_arguments(arguments, directory, records, ancestors=()):
    expanded = []
    for argument in arguments:
        if not argument.startswith('@'):
            expanded.append(argument)
            continue
        path = Path(argument[1:]).resolve()
        if path in ancestors or len(ancestors) >= 8:
            raise ValueError('Recursive linker response files')
        data = path.read_bytes()
        if len(data) > 64 * 1024 * 1024:
            raise ValueError('Linker response file is too large')
        retained = directory / f'response-{len(records)}.rsp'
        retained.write_bytes(data)
        records.append({'original': str(path), 'retained': retained.name,
                        'sha256': hashlib.sha256(data).hexdigest()})
        text = data.decode('utf-16') if data.startswith((b'\xff\xfe', b'\xfe\xff')) else data.decode('utf-8-sig')
        expanded.extend(retain_arguments(response_arguments(text), directory, records, (*ancestors, path)))
    return expanded


def linked_output(arguments):
    for index, argument in enumerate(arguments):
        if argument.lower().startswith('/out:'):
            return Path(argument[5:])
        if argument == '-o' and index + 1 < len(arguments):
            return Path(arguments[index + 1])
    return None


def retain_native_inputs(arguments, trace, directory):
    """Capture temporary import libraries before rustc removes its link directory."""
    search = [Path.cwd()]
    search.extend(Path(value) for value in os.environ.get('LIB', '').split(os.pathsep) if value)
    for argument in arguments:
        if argument.lower().startswith('/libpath:'):
            search.append(Path(argument[9:]))
    found = {}
    searched = {}

    def resolve(value):
        value = value.strip().strip('"').removesuffix(':')
        path = Path(value)
        candidates = [path] if path.is_absolute() else [searched.get(path.name.casefold()), *(root / path for root in search)]
        return next((candidate.resolve() for candidate in candidates
                     if candidate is not None and candidate.is_file()), None)

    for argument in arguments:
        if argument.lower().endswith(('.lib', '.rlib', '.a')):
            path = resolve(argument)
            if path is not None:
                found[path] = None
    for line in trace.splitlines():
        match = re.search(r'(?i)\b(Searching|Loaded)\s+(.+?\.(?:lib|rlib))(?=[:(\s]|$)', line)
        if not match:
            continue
        path = resolve(match[2])
        if path is not None:
            searched[path.name.casefold()] = path
            if match[1].lower() == 'loaded':
                found[path] = None
    records = []
    temporary = Path(os.environ.get('TMPDIR') or os.environ.get('TEMP') or '/tmp').resolve()
    for path in found:
        record = {'path': str(path), 'sha256': digest(path)}
        if path.suffix.lower() == '.lib' and path.is_relative_to(temporary) and not path.is_relative_to(directory.resolve()):
            if path.stat().st_size > 32 * 1024 * 1024:
                raise ValueError('Unexpectedly large temporary native import library')
            copied = directory / 'native-inputs' / f'{len(records)}-{path.name}'
            copied.parent.mkdir(exist_ok=True)
            copied.write_bytes(path.read_bytes())
            record['retained'] = copied.relative_to(directory).as_posix()
        records.append(record)
    return records


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--driver', required=True)
    parser.add_argument('--directory', required=True, type=Path)
    parser.add_argument('--link-map', required=True, type=Path)
    parser.add_argument('--rustc-commit', required=True)
    parser.add_argument('arguments', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    arguments = args.arguments[1:] if args.arguments[:1] == ['--'] else args.arguments
    args.directory.mkdir(parents=True, exist_ok=True)
    record_path = args.directory / 'link-record.json'
    record_path.unlink(missing_ok=True)
    response_files = []
    expanded = retain_arguments(arguments, args.directory, response_files)
    command = [args.driver, *arguments]
    command_record = {'command': command, 'expanded_arguments': expanded,
                      'response_files': response_files}
    (args.directory / 'link-command.json').write_text(json.dumps(command_record, indent=2) + '\n')
    result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    trace = args.directory / 'link-trace.txt'
    trace.write_bytes(result.stdout)
    sys.stdout.buffer.write(result.stdout)
    if result.returncode:
        return result.returncode
    output = linked_output(expanded)
    # Compiler driver probes may have no output; only actual links produce evidence.
    if output is not None:
        if not output.is_file() or not args.link_map.is_file():
            raise ValueError('Successful link did not produce its binary and map')
        binary_hash = digest(output)
        record = {**command_record, 'input_files': retain_native_inputs(expanded, result.stdout.decode(errors='replace'), args.directory), 'binary_sha256': binary_hash,
                  'linked_binary_sha256': binary_hash, 'rustc_commit': args.rustc_commit,
                  'link_map_sha256': digest(args.link_map), 'link_trace_sha256': digest(trace)}
        record_path.write_text(json.dumps(record, indent=2) + '\n')
    return 0


if __name__ == '__main__':
    sys.exit(main())
