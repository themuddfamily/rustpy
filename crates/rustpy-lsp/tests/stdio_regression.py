#!/usr/bin/env python3
"""Run against a built launcher: python3 stdio_regression.py /path/to/rustpy.

Uses actual stdio and temporary configuration/project directories; no display.
"""
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading
import time

BINARY = str(Path(sys.argv[1]).resolve())


class Client:
    def __init__(self, root, document_changes=False, global_before=True):
        self.root = root
        self.messages = queue.Queue()
        self.saved = {}
        self.stderr = open(root / 'stderr.log', 'wb')
        config, log = root / 'config', root / 'logs'
        args = ['--config-dir', str(config), 'lsp', '--log-dir', str(log)] if global_before else ['lsp', '--config-dir', str(config), '--log-dir', str(log)]
        self.process = subprocess.Popen([BINARY, *args], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.stderr, env={**os.environ, 'DISPLAY': '', 'WAYLAND_DISPLAY': ''})
        threading.Thread(target=self.read, daemon=True).start()
        self.send('initialize', {'processId': None, 'rootUri': root.as_uri(), 'capabilities': {'workspace': {'workspaceEdit': {'documentChanges': document_changes}}}, 'initializationOptions': {'interpreter': 'none', 'diagnosticsDelay': 10000, 'settings': {'inspections': {'grammar': False, 'spelling': False}}}}, 1)
        assert 'result' in self.response(1)
        self.send('initialized', {})
        assert (log / 'rustpy.log').exists(), 'global log-dir was applied before logging'

    def read(self):
        try:
            while True:
                headers = {}
                while True:
                    line = self.process.stdout.readline()
                    if not line:
                        return
                    if line == b'\r\n':
                        break
                    key, value = line.decode().strip().split(':', 1)
                    headers[key.lower()] = value.strip()
                self.messages.put(json.loads(self.process.stdout.read(int(headers['content-length']))))
        except Exception as error:
            self.messages.put({'reader_error': str(error)})

    def send(self, method, params, request_id=None):
        message = {'jsonrpc': '2.0', 'method': method, 'params': params}
        if request_id is not None:
            message['id'] = request_id
        body = json.dumps(message).encode()
        self.process.stdin.write(f'Content-Length: {len(body)}\r\n\r\n'.encode() + body)
        self.process.stdin.flush()

    def response(self, request_id):
        if request_id in self.saved:
            return self.saved.pop(request_id)
        deadline = time.monotonic() + 90
        while True:
            message = self.messages.get(timeout=max(.01, deadline - time.monotonic()))
            assert 'reader_error' not in message, message
            if 'method' in message:
                if 'id' in message:
                    body = json.dumps({'jsonrpc': '2.0', 'id': message['id'], 'result': None}).encode()
                    self.process.stdin.write(f'Content-Length: {len(body)}\r\n\r\n'.encode() + body)
                    self.process.stdin.flush()
                continue
            if message.get('id') == request_id:
                return message
            self.saved[message.get('id')] = message

    def open(self, name, text):
        self.send('textDocument/didOpen', {'textDocument': {'uri': (self.root / name).as_uri(), 'languageId': 'python', 'version': 1, 'text': text}})

    def close(self):
        try:
            self.send('shutdown', None, 9000)
            assert 'result' in self.response(9000)
            self.send('exit', None)
            self.process.stdin.close()
            assert self.process.wait(timeout=20) == 0
        finally:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait()
            self.stderr.close()


def unsaved_and_dispatch(root):
    (root / 'helper.py').write_text('class OldChoice: pass\n')
    (root / 'main.py').write_text('New')
    client = Client(root)
    try:
        client.send('workspace/symbol', {'query': 'OldChoice'}, 2)
        assert client.response(2)['result'], 'disk symbols are available'
        client.open('helper.py', 'class NewChoice: pass\n')
        client.open('main.py', 'New')
        client.send('textDocument/completion', {'textDocument': {'uri': (root / 'main.py').as_uri()}, 'position': {'line': 0, 'character': 3}}, 3)
        items = client.response(3)['result']['items']
        choice = next(i for i in items if i['label'] == 'NewChoice')
        assert choice['additionalTextEdits'][0]['newText'] == 'from helper import NewChoice\n'
        assert not any(i['label'] == 'OldChoice' for i in items)
    finally:
        client.close()


def stale_rename(root, document_changes):
    main, helper = 'class Target: pass\n', 'from main import Target\nx=Target()\n'
    (root / 'main.py').write_text(main)
    (root / 'helper.py').write_text(helper)
    # Occupy request workers so the rename completes after the edit notification.
    for i in range(2500):
        (root / f'z{i:04}.py').write_text('from main import Target\n' + '\n'.join(f'x{j}=Target()' for j in range(20)) + '\n')
    client = Client(root, document_changes, global_before=False)
    try:
        client.open('main.py', main)
        client.open('helper.py', helper)
        position = {'textDocument': {'uri': (root / 'main.py').as_uri()}, 'position': {'line': 0, 'character': 8}}
        for request_id in range(2, 10):
            client.send('textDocument/references', {**position, 'context': {'includeDeclaration': True}}, request_id)
        client.send('textDocument/rename', {**position, 'newName': 'Changed'}, 10)
        client.send('textDocument/didChange', {'textDocument': {'uri': (root / 'helper.py').as_uri(), 'version': 2}, 'contentChanges': [{'text': '\n' * 10 + helper}]})
        client.send('textDocument/documentSymbol', {'textDocument': {'uri': (root / 'helper.py').as_uri()}}, 11)
        response = client.response(10)
        assert response.get('error', {}).get('code') == -32801, response
        assert 'result' in client.response(11), 'new generation still responds'
    finally:
        client.close()


with tempfile.TemporaryDirectory(prefix='rustpy-stdio-regression-') as temporary:
    root = Path(temporary)
    for name, test in [('unsaved', unsaved_and_dispatch), ('unversioned', lambda r: stale_rename(r, False)), ('versioned', lambda r: stale_rename(r, True))]:
        folder = root / name
        folder.mkdir()
        test(folder)
        print(name, 'PASS', flush=True)
    project = root / 'inspect'
    project.mkdir()
    (project / 'generated.py').write_text('__all__ = ' + ' + '.join(["[\'a\']"] * 9500) + '\n')
    result = subprocess.run([BINARY, '--config-dir', str(root / 'inspect-config'), 'inspect', str(project), 'Project Default', str(root / 'report'), '-format', 'json'], capture_output=True, timeout=90)
    assert result.returncode in (0, 1), (result.returncode, result.stderr.decode())
    assert list((root / 'report').glob('*.json')), 'inspection report was produced'
    for args, code in [(['lsp', '--help'], 0), (['lsp', '--unknown'], 2)]:
        result = subprocess.run([BINARY, *args], capture_output=True, timeout=20)
        assert result.returncode == code, (args, result.returncode, result.stderr.decode())
    print('inspect and CLI help/error PASS', flush=True)
