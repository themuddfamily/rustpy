"""Thread/task timeline instrumenter. No target code runs during discovery.

Execution spans describe Python call lifetimes, not CPU samples. Lock spans
measure actual acquire attempts; async awaits are coroutine suspend/resume pairs.
"""
import argparse
import asyncio
import dis
import inspect
import itertools
import json
import os
import runpy
import sys
import threading
import time
import weakref


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--trace", required=True)
    parser.add_argument("--options", required=True)
    parser.add_argument("target", nargs=argparse.REMAINDER)
    options = parser.parse_args()
    target = options.target
    if target and target[0] == "--":
        target = target[1:]
    if not target:
        parser.error("a Python script or -m module is required")
    config = json.loads(options.options)
    roots = [os.path.normcase(os.path.realpath(path)) for path in config.get("roots", [])]
    own_file = os.path.normcase(os.path.realpath(__file__))
    max_events = min(max(int(config.get("max_events", 100000)), 1000), 1000000)
    max_lanes = min(max(int(config.get("max_lanes", 256)), 1), 1024)
    clock = time.perf_counter
    started = clock()
    output = open(options.trace, "w", encoding="utf-8", buffering=1)
    writer_lock = threading.RLock()
    local = threading.local()
    sequence = itertools.count(1)
    lane_sequence = itertools.count(1)
    threads = weakref.WeakKeyDictionary()
    tasks = weakref.WeakKeyDictionary()
    calls = {}
    awaits = {}
    c_waits = {}
    depths = {}
    selected_cache = {}
    interrupted = False
    original_join = threading.Thread.join
    counters = {"events": 0, "dropped": 0, "lanes": 0, "closing": False, "failed": False}

    def selected_file(path):
        if path in selected_cache:
            return selected_cache[path]
        if not path or path.startswith("<"):
            return False
        normalized = os.path.normcase(os.path.realpath(path))
        selected = normalized != own_file and (not config.get("project_only", True) or
                    any(normalized == root or normalized.startswith(root + os.sep) for root in roots))
        selected_cache[path] = selected
        return selected

    def emit(kind, **values):
        if counters["failed"] or getattr(local, "writing", False) or (counters["closing"] and kind != "finish"):
            return
        local.writing = True
        try:
            with writer_lock:
                if counters["events"] >= max_events and kind != "finish":
                    counters["dropped"] += 1
                    if counters["dropped"] == 1:
                        output.write(json.dumps({"kind": "truncated", "time": max(0.0, clock() - started), "dropped": 1}) + "\n")
                    return
                values.update(kind=kind, time=max(0.0, clock() - started))
                output.write(json.dumps(values, ensure_ascii=True, separators=(",", ":")) + "\n")
                counters["events"] += 1
        except OSError:
            counters["failed"] = True
        finally:
            local.writing = False

    def thread_lane():
        thread = threading.current_thread()
        lane = threads.get(thread)
        if lane:
            return lane
        if counters["lanes"] >= max_lanes:
            counters["dropped"] += 1
            return None
        lane = "thread:%s" % next(lane_sequence)
        threads[thread] = lane
        counters["lanes"] += 1
        emit("lane", lane=lane, name=thread.name, lane_kind="thread")
        return lane

    def _current_lane():
        thread = thread_lane()
        if config.get("asyncio", True):
            try:
                task = asyncio.current_task()
            except RuntimeError:
                task = None
            if task is not None:
                lane = tasks.get(task)
                if not lane:
                    if counters["lanes"] >= max_lanes:
                        counters["dropped"] += 1
                        return None
                    lane = "task:%s" % next(lane_sequence)
                    tasks[task] = lane
                    counters["lanes"] += 1
                    name = task.get_name() if hasattr(task, "get_name") else str(task)
                    emit("lane", lane=lane, name=name, lane_kind="task", parent=thread)
                return lane
        return thread if config.get("threads", True) else None

    def current_lane():
        previous = getattr(local, "resolving", False)
        local.resolving = True
        try:
            return _current_lane()
        finally:
            local.resolving = previous

    def site(frame):
        for _ in range(48):
            if frame is None:
                return None
            if selected_file(frame.f_code.co_filename):
                return (os.path.abspath(frame.f_code.co_filename), frame.f_lineno)
            frame = frame.f_back
        return None

    def begin(lane, name, phase, location, resource=None, depth=0):
        if not lane or not location:
            return None
        span = next(sequence)
        emit("begin", span=span, lane=lane, name=name, phase=phase,
             file=location[0], line=location[1], resource=resource, depth=depth)
        return span

    def end(span, lane):
        if span:
            emit("end", span=span, lane=lane)

    def record_profile(frame, event, argument):
        if getattr(local, "writing", False) or getattr(local, "resolving", False) or counters["closing"]:
            return
        is_c = event in ("c_call", "c_return", "c_exception")
        if is_c:
            if not config.get("locks", True):
                return
            owner = getattr(argument, "__self__", None)
            name = getattr(argument, "__name__", "")
            owner_type = type(owner).__name__
            if owner_type not in ("lock", "RLock") or name not in ("acquire", "__enter__"):
                return
            location = site(frame)
            if not location:
                return
        else:
            if not selected_file(frame.f_code.co_filename):
                return
            location = (os.path.abspath(frame.f_code.co_filename), frame.f_lineno)
        lane = current_lane()
        if not lane:
            return
        key = (lane, id(frame))
        if event == "call":
            suspended = awaits.pop(key, None)
            if suspended:
                end(suspended, lane)
            depth = depths.get(lane, 0)
            calls[key] = begin(lane, frame.f_code.co_name, "execution", location, depth=depth)
            depths[lane] = depth + 1
        elif event == "return":
            end(calls.pop(key, None), lane)
            depths[lane] = max(0, depths.get(lane, 1) - 1)
            flags = frame.f_code.co_flags
            if config.get("asyncio", True) and flags & inspect.CO_COROUTINE:
                instruction = frame.f_code.co_code[frame.f_lasti] if 0 <= frame.f_lasti < len(frame.f_code.co_code) else 0
                if dis.opname[instruction] in ("YIELD_VALUE", "YIELD_FROM", "SEND"):
                    awaits[key] = begin(lane, "await in " + frame.f_code.co_name, "await", location)
        elif config.get("locks", True) and event in ("c_call", "c_return", "c_exception"):
            owner = getattr(argument, "__self__", None)
            name = getattr(argument, "__name__", "")
            owner_type = type(owner).__name__
            if owner_type not in ("lock", "RLock") or name not in ("acquire", "__enter__"):
                return
            lock_key = (lane, id(frame), id(owner), name)
            if event == "c_call":
                c_waits[lock_key] = begin(lane, owner_type + ".acquire", "lock_wait", location,
                                          resource="lock:%x" % id(owner))
            else:
                end(c_waits.pop(lock_key, None), lane)

    def profile(frame, event, argument):
        # Instrumentation failures must never replace exceptions in target code.
        try:
            record_profile(frame, event, argument)
        except Exception:
            counters["dropped"] += 1

    original_async = []
    if config.get("asyncio", True) and config.get("locks", True):
        def instrument_acquire(cls):
            original = cls.acquire
            async def acquire(lock, *args, **kwargs):
                lane = current_lane()
                local.resolving = True
                try:
                    location = site(sys._getframe(1))
                finally:
                    local.resolving = False
                span = begin(lane, cls.__name__ + ".acquire", "lock_wait", location,
                             resource="async-lock:%x" % id(lock))
                try:
                    return await original(lock, *args, **kwargs)
                finally:
                    end(span, lane)
            original_async.append((cls, original))
            cls.acquire = acquire
        for cls in (asyncio.Lock, asyncio.Semaphore, asyncio.BoundedSemaphore):
            # BoundedSemaphore inherits acquire: capture the unmodified method
            # once, rather than stacking wrappers on its base class wrapper.
            if cls is asyncio.BoundedSemaphore:
                continue
            instrument_acquire(cls)
    previous = sys.getprofile()
    get_thread_profile = getattr(threading, "getprofile", lambda: None)
    previous_thread = get_thread_profile()
    sys.setprofile(profile)
    threading.setprofile(profile)
    if hasattr(threading, "setprofile_all_threads"):
        threading.setprofile_all_threads(profile)
    try:
        if target[0] == "-m":
            if len(target) < 2:
                raise SystemExit("-m requires a module")
            sys.argv = target[1:]
            runpy.run_module(target[1], run_name="__main__", alter_sys=True)
        else:
            script = os.path.abspath(target[0])
            sys.argv = [script] + target[1:]
            sys.path[0] = os.path.dirname(script)
            runpy.run_path(script, run_name="__main__")
    except KeyboardInterrupt:
        interrupted = True
        raise
    finally:
        # Match normal interpreter shutdown: non-daemon workers finish before
        # the trace is closed, including workers started by other workers.
        while True:
            workers = [thread for thread in threading.enumerate()
                       if thread is not threading.current_thread() and not thread.daemon]
            if not workers:
                break
            for thread in workers:
                original_join(thread)
        counters["closing"] = True
        sys.setprofile(previous)
        threading.setprofile(previous_thread)
        if hasattr(threading, "setprofile_all_threads"):
            threading.setprofile_all_threads(previous_thread)
            sys.setprofile(previous)
        for cls, original in original_async:
            cls.acquire = original
        emit("finish", dropped=counters["dropped"], terminated=interrupted)
        try:
            output.close()
        except OSError:
            pass


if __name__ == "__main__":
    main()
