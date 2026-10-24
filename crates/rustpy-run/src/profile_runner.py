"""RustPy cProfile/yappi runner and standard pstat conversion helper."""
import cProfile
import json
import marshal
import os
import pstats
import runpy
import signal
import sys
import time
import traceback
PREFIX = '##rustpy-profile## '


def emit(stats, out, elapsed=0.0, exit_code=0, threads=None, engine='pstat', sampling=False):
    total_time = 0.0
    total_calls = 0
    for (file, line, name), (cc, nc, tt, ct, callers) in stats.stats.items():
        total_time += tt
        total_calls += nc
        callers_list = []
        for (cfile, cline, cname), value in callers.items():
            if isinstance(value, tuple):
                ccc, cnc, ctt, cct = value
            else:
                ccc = cnc = int(value)
                ctt = cct = 0.0
            callers_list.append(dict(file=cfile, line=cline, name=cname, ncalls=cnc, primitive_calls=ccc, tottime=ctt, cumtime=cct))
        out.write(PREFIX + json.dumps(dict(file=file, line=line, name=name, ncalls=nc, primitive_calls=cc, tottime=tt, cumtime=ct, callers=callers_list)) + '\n')
    out.write(PREFIX + json.dumps(dict(event='summary', total_time=total_time, total_calls=total_calls, elapsed=elapsed, exit_code=exit_code, engine=engine, sampling=sampling, threads=threads or [])) + '\n')
    out.flush()


def save_report(source, destination):
    with open(source, encoding='utf-8') as stream:
        report = json.load(stream)
    stats = {}
    for function in report['functions']:
        callers = {(item['file'], item['line'], item['name']): (item['primitive_calls'], item['ncalls'], item['tottime'], item['cumtime']) for item in function['callers']}
        stats[(function['file'], function['line'], function['name'])] = (function['primitive_calls'], function['ncalls'], function['tottime'], function['cumtime'], callers)
    with open(destination, 'wb') as stream:
        marshal.dump(stats, stream)


def emit_vmprof(path, out, elapsed=0.0, exit_code=0):
    import vmprof
    sampled = vmprof.read_profile(path)
    root = sampled.get_tree()
    if not elapsed:
        elapsed = sampled.get_runtime_in_microseconds() / 1000000.0
    period = elapsed / root.count if root.count else 0.0
    entries = {}
    pending = [(root, None)]
    while pending:
        node, parent = pending.pop()
        parts = node.name.split(':', 3)
        key = (parts[3], int(parts[2]), parts[1]) if len(parts) == 4 and parts[2].isdigit() else ('~', 0, node.name)
        entry = entries.setdefault(key, [0, 0, 0.0, 0.0, {}])
        count = int(round(node.count))
        own = max(0.0, node.self_count * period)
        total = node.count * period
        entry[0] += count
        entry[1] += count
        entry[2] += own
        entry[3] += total
        if parent is not None:
            caller = entry[4].setdefault(parent, [0, 0, 0.0, 0.0])
            caller[0] += count
            caller[1] += count
            caller[2] += own
            caller[3] += total
        pending.extend((child, key) for child in node.children.values())
    class Converted:
        stats = {key: tuple(value[:4]) + ({caller: tuple(counts) for caller, counts in value[4].items()},) for key, value in entries.items()}
    emit(Converted(), out, elapsed, exit_code, engine='vmprof', sampling=True)


def main():
    argv = sys.argv[1:]
    if argv and argv[0] == '--rustpy-import':
        emit(pstats.Stats(argv[1]), sys.stdout)
        return 0
    if argv and argv[0] == '--rustpy-vmprof-import':
        emit_vmprof(argv[1], sys.stdout)
        return 0
    if argv and argv[0] == '--rustpy-save':
        save_report(argv[1], argv[2])
        return 0
    mode = 'script'
    if argv and argv[0] == '-m':
        mode = 'module'
        argv = argv[1:]
    if not argv:
        sys.stderr.write('profile_runner: missing target\n')
        return 2
    if hasattr(signal, 'SIGBREAK'):
        signal.signal(signal.SIGBREAK, signal.default_int_handler)
    target, rest = argv[0], argv[1:]
    real_stdout = sys.__stdout__
    engine = os.environ.get('RUSTPY_PROFILE_ENGINE', 'cprofile')
    destination = os.environ.get('RUSTPY_PROFILE_PSTAT')
    threads = []
    exit_code = 0
    if engine == 'vmprof':
        import vmprof
        sampling_stream = open(destination, 'w+b')
        def enable():
            vmprof.enable(sampling_stream.fileno(), period=0.00099)
        def disable():
            vmprof.disable()
            sampling_stream.close()
    elif engine == 'yappi':
        import yappi
        yappi.clear_stats()
        yappi.set_clock_type(os.environ.get('RUSTPY_PROFILE_CLOCK', 'wall'))
        def enable():
            yappi.start(builtins=os.environ.get('RUSTPY_PROFILE_BUILTINS', '1') == '1', profile_threads=os.environ.get('RUSTPY_PROFILE_THREADS', '1') == '1')
        def disable():
            yappi.stop()
    else:
        profiler = cProfile.Profile(builtins=os.environ.get('RUSTPY_PROFILE_BUILTINS', '1') == '1')
        enable, disable = profiler.enable, profiler.disable
    start = time.perf_counter()
    try:
        enable()
        try:
            if mode == 'script':
                script = os.path.abspath(target)
                sys.argv = [script] + rest
                sys.path.insert(0, os.path.dirname(script))
                runpy.run_path(script, run_name='__main__')
            else:
                sys.argv = [target] + rest
                sys.path.insert(0, os.getcwd())
                runpy.run_module(target, run_name='__main__', alter_sys=True)
        finally:
            disable()
    except SystemExit as error:
        exit_code = error.code if isinstance(error.code, int) else (0 if error.code is None else 1)
    except KeyboardInterrupt:
        # Finish-and-collect is also how long-running web servers produce data.
        exit_code = 130
    except BaseException:
        traceback.print_exc()
        exit_code = 1
    elapsed = time.perf_counter() - start
    if engine == 'vmprof':
        emit_vmprof(destination, real_stdout, elapsed, exit_code)
        return exit_code
    if engine == 'yappi':
        threads = [dict(id=item.id, name=item.name, total_time=item.ttot, schedules=item.sched_count) for item in yappi.get_thread_stats()]
        yappi.get_func_stats().save(destination, type='pstat')
        stats = pstats.Stats(destination)
    else:
        stats = pstats.Stats(profiler)
        if destination:
            stats.dump_stats(destination)
    emit(stats, real_stdout, elapsed, exit_code, threads, engine)
    return exit_code


if __name__ == '__main__':
    sys.exit(main())
