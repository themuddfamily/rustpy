"""Explicit fixture-only real grpcio server; no reflection/vendor endpoints."""
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import sys
import time
import grpc
from google.protobuf import descriptor_pb2, descriptor_pool, message_factory

root = Path(sys.argv[1])
descriptor = descriptor_pb2.FileDescriptorProto(name='acceptance.proto', package='rustpy.acceptance', syntax='proto3')
message = descriptor.message_type.add(name='Text')
message.field.add(name='value', number=1, label=1, type=9)
service = descriptor.service.add(name='Service')
for name in ['Echo', 'Fail', 'Wait']:
    service.method.add(name=name, input_type='.rustpy.acceptance.Text', output_type='.rustpy.acceptance.Text')
pool = descriptor_pool.DescriptorPool()
pool.Add(descriptor)
Text = message_factory.GetMessageClass(pool.FindMessageTypeByName('rustpy.acceptance.Text'))


def event(kind, **values):
    values.update(kind=kind, pid=os.getpid())
    fd = os.open(root / 'events.jsonl', os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    try:
        os.write(fd, (json.dumps(values) + '\n').encode())
        os.fsync(fd)
    finally:
        os.close(fd)


def handler(name):
    def call(request, context):
        metadata = dict(context.invocation_metadata())
        event(name, value=request.value, metadata=metadata)
        if metadata.get('x-rustpy-acceptance') != 'actual-grpc-metadata':
            context.abort(grpc.StatusCode.UNAUTHENTICATED, 'ACTUAL_METADATA_REQUIRED')
        if name == 'Fail':
            context.abort(grpc.StatusCode.INVALID_ARGUMENT, 'ACTUAL_GRPC_INVALID_ARGUMENT')
        if name == 'Wait':
            while context.is_active():
                time.sleep(0.01)
            event('wait-retired')
            return Text(value='cancelled')
        return Text(value='echo:' + request.value)
    return grpc.unary_unary_rpc_method_handler(call, request_deserializer=Text.FromString, response_serializer=Text.SerializeToString)


with ThreadPoolExecutor(max_workers=2) as executor:
    server = grpc.server(executor)
    server.add_generic_rpc_handlers((grpc.method_handlers_generic_handler('rustpy.acceptance.Service', {name: handler(name) for name in ['Echo', 'Fail', 'Wait']}),))
    port = server.add_insecure_port('127.0.0.1:0')
    if not port:
        raise RuntimeError('owned loopback port allocation failed')
    server.start()
    ready = root / 'ready.json'
    staged = root / 'ready.tmp'
    staged.write_text(json.dumps(dict(pid=os.getpid(), port=port)))
    staged.replace(ready)
    while not (root / 'stop').exists():
        time.sleep(0.01)
    server.stop(0).wait()
event('server-retired')
