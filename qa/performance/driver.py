#!/usr/bin/env python3
"""Final acceptance driver. Default prints a plan; only --execute starts work.

Examples (FINAL GATE ONLY, not feature implementation):
  python3 qa/performance/driver.py native10k --binary ./target/debug/rustpy --output "$TMPDIR/perf-10k" --execute
  python3 qa/performance/driver.py native10k --profile both --binary ./target/debug/rustpy --release-binary ./target/release/rustpy --output "$TMPDIR/perf-10k" --execute
  python3 qa/performance/driver.py memory100k --binary ./target/debug/rustpy --output "$TMPDIR/perf-100k" --seconds 110 --execute
  python3 qa/performance/driver.py largefile --binary ./target/debug/rustpy --output "$TMPDIR/perf-large" --execute
  python3 qa/performance/driver.py million --output "$TMPDIR/perf-million" --seconds 300 --execute
Native GUI execution is ONLY through scripts/smoke.sh and its private Xvfb;
inherited real DISPLAY is removed. smoke.sh launches ./target/<profile>/rustpy
(RUSTPY_SMOKE_PROFILE) with a forced software renderer; --profile both measures
the debug and release builds and labels the release result as the one the
roadmap latency targets apply to. Hardware acceptance is unavailable here.
The capture waits for the whole input script (RUSTPY_SMOKE_AWAIT_SCRIPT) up to
--seconds; script steps that never ran are reported as "not reached" (fail).
--cores is the CPU-affinity cap (default 10, the shared PC's current cap) and
--workers the background analysis worker count (default cores-1, the IDE's
own policy of reserving one core for the UI thread).
Million builds its release test binary first, outside the memory watchdog
(or takes --test-binary), then runs only that binary under the watchdog.
Fresh generated-project-only trust/config/system directories are isolated.
Evidence/failures are retained; no build or installation is implicit in native
stages.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import re
import subprocess
import sys
import time
from acceptance import evaluate

ROOT = Path(__file__).resolve().parents[2]
SMOKE = ROOT / "scripts/smoke.sh"
# The roadmap latency/memory targets describe what users run: a release build.
ROADMAP_TARGET_PROFILE = "release"
# Default CPU cap: the shared PC's current build/test cap (raised from 2).
DEFAULT_CORES = 10
MILLION_TEST = "million_line_parallel_project_analysis_retains_all_diagnostics_with_a_small_disk_cache"


def smoke_binary(profile):
    return ROOT / "target" / profile / "rustpy"


def native_contract(seconds, renderer):
    """Returns (minimum capture delay, capture deadline) for a native run."""
    if renderer != "software":
        raise ValueError("unavailable: scripts/smoke.sh forces software GL; hardware measurement needs a separately authorized native path")
    if not 60 <= seconds <= 900:
        raise ValueError("unavailable: native smoke runs require 60..900s")
    # smoke.sh's hard timeout is the run budget; capture at least 5 s before.
    return min(seconds-20, 90), seconds-5


def worker_plan(cores, workers=None):
    """Explicit CPU cap and background analysis workers (default cores-1)."""
    if cores < 1:
        raise ValueError("--cores must be at least 1")
    workers = max(1, cores-1) if workers is None else workers
    if workers < 1:
        raise ValueError("--workers must be at least 1")
    return cores, workers


def measured_binary(requested, actual):
    if requested is None or not requested.is_file() or not actual.is_file():
        raise ValueError(f"explicit existing binary option and {actual} used by smoke.sh are required; build centrally first")
    actual_sha = hashlib.sha256(actual.read_bytes()).hexdigest()
    if hashlib.sha256(requested.read_bytes()).hexdigest() != actual_sha:
        raise ValueError(f"requested binary SHA does not match {actual} actually launched by smoke.sh")
    return actual.resolve(), actual_sha


def smoke_environment(environment, delay, profile="debug", timeout=120, deadline=None):
    env = environment.copy()
    env["RUSTPY_SMOKE_DELAY"] = str(delay)
    env["RUSTPY_SMOKE_UNTRUSTED"] = "1"
    env["RUSTPY_SMOKE_PROFILE"] = profile
    env["RUSTPY_SMOKE_TIMEOUT"] = str(int(timeout))
    if deadline is not None:
        env["RUSTPY_SMOKE_AWAIT_SCRIPT"] = "1"
        env["RUSTPY_SMOKE_DEADLINE"] = str(deadline)
    for key in ("RUSTPY_TRUST_ALL","DISPLAY","WAYLAND_DISPLAY"):
        env.pop(key,None)
    return env


def smoke_command(project, active, script, output):
    return [str(SMOKE),"--new-window",str(project),"--open-file",str(active),"--input-script",str(script),"--screenshot",str(output/"final.png")]



def controls(output, cores=2, workers=1):
    if sys.platform != "linux" or not hasattr(os, "sched_setaffinity"):
        raise ValueError("unavailable: Linux affinity + /proc driver; native Windows/macOS evidence must use native resource/job/RSS controls")
    target = Path(os.environ.get("CARGO_TARGET_DIR", ""))
    temporary = Path(os.environ.get("TMPDIR", ""))
    if not os.environ.get("CARGO_TARGET_DIR") or not target.is_dir():
        raise ValueError("set existing shared CARGO_TARGET_DIR; no alternate build cache")
    if not os.environ.get("TMPDIR") or not temporary.is_dir() or temporary.resolve().parent == Path("/tmp"):
        raise ValueError("set an existing isolated TMPDIR nested inside an existing RustPy temporary root")
    if not output.resolve().is_relative_to(temporary.resolve()) or output.resolve() == temporary.resolve():
        raise ValueError("output must be a child of the isolated TMPDIR")
    if not output.is_dir() or any(output.iterdir()):
        raise ValueError("explicitly create an empty output directory first; prior evidence is never overwritten")
    cpus = sorted(os.sched_getaffinity(0))[:cores]
    if len(cpus) < cores:
        raise ValueError(f"--cores {cores} requested but only {len(cpus)} CPUs are allowed")
    os.sched_setaffinity(0, cpus)
    # Ensure niceness >=10; do not cumulatively penalize an already capped shell.
    if os.getpriority(os.PRIO_PROCESS,0) < 10:
        os.nice(10-os.getpriority(os.PRIO_PROCESS,0))
    env = os.environ.copy()
    env.update(CARGO_BUILD_JOBS=str(cores), RAYON_NUM_THREADS=str(cores), RUST_TEST_THREADS=str(cores),
               RUSTPY_BACKGROUND_THREADS=str(workers))
    return env, cpus


def scaled_wait(seconds, scale):
    return f"wait {seconds*scale:g}"


def input_commands(stage, scale=1.0):
    """The native input script, with `mark` steps recorded in the trace.

    Returns (commands, required marks). A mark that never appears in the trace
    is a step that was not reached (a slow UI, or the capture deadline).
    """
    commands = ["mark start", scaled_wait(3, scale), "key End ctrl"]
    if stage != "largefile":
        commands.append("mark typing")
        for _ in range(25):
            commands += ["key Space ctrl", scaled_wait(.15, scale), "key Escape", "type x", "key Backspace", scaled_wait(.35, scale)]
        for mark, action in (("power-save-on","Power Save Mode"), ("power-save-off","Power Save Mode"), ("invalidate-caches","Invalidate Caches")):
            commands += ["mark "+mark, "key A ctrl shift", scaled_wait(.2, scale), "typefast " + action, scaled_wait(.2, scale), "key Enter", scaled_wait(.5, scale)]
            if action == "Power Save Mode": commands += ["type x", "key Backspace", scaled_wait(1, scale)]
        # Invalidate Caches opens its dialog; Enter confirms its default
        # button, "Apply selected repairs" (file system/analysis/symbol caches).
        commands += ["mark invalidate-caches-apply", "key Enter", scaled_wait(1, scale)]
    else:
        commands += ["mark navigate", "key Home ctrl", "key End ctrl", scaled_wait(1, scale), "key Home ctrl"]
    # Save only our generated active document so coordinated native quit does
    # not stop at an unsaved-edits prompt after the reversible typing samples.
    commands += ["mark save", "key S ctrl", scaled_wait(.5, scale), "mark end"]
    return commands, [c.split(" ",1)[1] for c in commands if c.startswith("mark ")]


def prepare(output, stage, scale=1.0):
    """Bounded workload creation is an explicit part of --execute only."""
    project = output / "project"
    project.mkdir()
    count = {"native10k":10_000,"memory100k":100_000,"largefile":1}[stage]
    digest = hashlib.sha256()
    for number in range(count):
        directory = project / f"package_{number//100:04}"
        directory.mkdir(exist_ok=True)
        source = f"def compute_{number}(value: int) -> int:\n    return value + {number}\n"
        (directory/f"module_{number:06}.py").write_text(source)
        digest.update(source.encode())
    active = project / "active.py"
    if stage == "largefile":
        text = "value = 1234567890123456789012345678901234567890\n" * 120_000
    else:
        # Real 5k-line document; no million-line single-file surrogate.
        text = "".join(f"value_{i:04} = {i}\n" for i in range(5000)) + "result = value"
    active.write_text(text)
    script = output / "input.txt"
    commands, marks = input_commands(stage, scale)
    script.write_text("\n".join(commands)+"\n")
    inventory = dict(project_files=count+1, active_lines=len(text.splitlines()), active_bytes=len(text.encode()), generated_content_sha256=digest.hexdigest())
    (output/"inventory.json").write_text(json.dumps(inventory,indent=2))
    return project, active, script, inventory, marks


def proc_snapshot():
    parents, resident = {}, {}
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit(): continue
        try:
            status = (entry/"status").read_text()
            fields = dict(line.split(":",1) for line in status.splitlines() if ":" in line)
            parents[int(entry.name)] = int(fields["PPid"].strip())
            resident[int(entry.name)] = int(fields.get("VmRSS","0 kB").split()[0])*1024
        except (FileNotFoundError, PermissionError, ProcessLookupError, ValueError, KeyError):
            continue
    return parents, resident


def descendants(root_pid, parents):
    owned = {root_pid}
    while True:
        expanded = owned | {pid for pid,parent in parents.items() if parent in owned}
        if expanded == owned: return owned
        owned = expanded


def process_tree_rss(root_pid, parents, resident):
    return sum(resident.get(pid,0) for pid in descendants(root_pid,parents))


def trace_pid(trace):
    try:
        with trace.open() as source:
            first = source.readline(65536)
        row = json.loads(first)
        pid = row.get("pid")
        return pid if row.get("schema") == 1 and type(pid) is int and pid > 0 else None
    except (OSError,ValueError):
        return None


def identify_app(pid, wrapper_pid, parents, expected_sha):
    if pid not in descendants(wrapper_pid,parents) or pid == wrapper_pid:
        raise ValueError("trace PID is not a RustPy descendant of the owned smoke wrapper")
    executable = Path(f"/proc/{pid}/exe")
    if hashlib.sha256(executable.read_bytes()).hexdigest() != expected_sha:
        raise ValueError("trace process executable SHA does not match the measured smoke binary")
    # Linux process birth, rounded down to its kernel clock tick. This keeps
    # cold RustPy launch distinct from Xvfb/wrapper startup (also retained).
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")",1)[1].split()
    born_ticks = int(fields[19])
    clock_hz = os.sysconf("SC_CLK_TCK")
    epoch_ns = time.time_ns() - int((time.clock_gettime(time.CLOCK_BOOTTIME)-born_ticks/clock_hz)*1e9)
    return dict(app_pid=pid, app_pid_verified=True, app_start_epoch_ns=epoch_ns,
                app_start_precision_ms=1000/clock_hz, app_executable_sha256=expected_sha)


def terminate(process):
    if process.poll() is not None: return
    os.killpg(process.pid,signal.SIGTERM)
    try: process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid,signal.SIGKILL)
        process.wait(timeout=5)


def test_executable(messages, test="million_line_project"):
    """The test binary path from `cargo test --no-run --message-format=json`."""
    found = None
    for line in messages:
        try: message = json.loads(line)
        except ValueError: continue
        if (message.get("reason") == "compiler-artifact" and message.get("target",{}).get("name") == test
                and "test" in message.get("target",{}).get("kind",[]) and message.get("executable")):
            found = message["executable"]
    if found is None:
        raise ValueError(f"cargo reported no executable for the {test} test")
    return Path(found)


def build_million(output, env):
    """Compiles the release test binary outside the memory watchdog.

    rustc easily exceeds the 1 GB whole-tree safety limit, which is a limit
    on the measured workload, not on its build.
    """
    command = ["cargo","test","--locked","--release","-p","rustpy-analysis","--test","million_line_project","--no-run","--message-format=json"]
    with (output/"build.jsonl").open("wb") as stdout, (output/"build.log").open("wb") as stderr:
        built = subprocess.run(command,cwd=ROOT,env=env,stdout=stdout,stderr=stderr)
    if built.returncode != 0:
        raise ValueError(f"million test build failed ({built.returncode}); see build.log")
    return command, test_executable((output/"build.jsonl").read_text(errors="replace").splitlines())


def million_result(raw, returncode, metadata):
    # The real Rust workload asserts >=1M lines, diagnostic completeness,
    # cache limits and stable IDs. Preserve raw output; no substitute test.
    measured = re.search(r"(\d+) lines \((\d+) nonblank Python lines\) / 128 files", raw)
    complete = measured is not None and int(measured[1]) >= 1_000_000 and int(measured[2]) >= 900_000 and "1 passed; 0 failed" in raw
    ok = complete and returncode == 0 and not metadata["timed_out"] and not metadata.get("memory_limit_exceeded")
    return dict(status="pass" if ok else "fail", observed_lines=int(measured[1]) if measured else None,
                cores=len(metadata["cpus"]), workers=metadata["workers"], limit_seconds=metadata["limit_seconds"],
                elapsed_seconds=metadata.get("elapsed_seconds"),
                boundary="existing million-line test; native latency/memory require separate stages")


def combined_result(results):
    """Debug and release results side by side; the roadmap target is release."""
    target = results.get(ROADMAP_TARGET_PROFILE)
    return {"schema":1,"scope":"one stage, both build profiles","roadmap_target_profile":ROADMAP_TARGET_PROFILE,
            "status":target["status"] if target else "fail",
            "profiles":{profile:dict(result, applies_to_roadmap_target=profile == ROADMAP_TARGET_PROFILE) for profile,result in results.items()}}


def run(args):
    output = args.output.resolve()
    cores, workers = worker_plan(args.cores, args.workers)
    env, cpus = controls(output, cores, workers)
    if args.stage == "million":
        result = run_million(args, output, env, cpus, workers)
    elif args.profile == "both":
        results = {}
        for profile in ("debug", "release"):
            (output/profile).mkdir()
            results[profile] = run_native(args, output/profile, env, cpus, workers, profile)
        result = combined_result(results)
    else:
        result = run_native(args, output, env, cpus, workers, args.profile)
    (output/"acceptance.json").write_text(json.dumps(result,indent=2))
    print(json.dumps(result,indent=2))
    return 0 if result["status"] == "pass" else 1


def base_metadata(args, env, cpus, workers):
    return dict(schema=1, stage=args.stage, renderer=args.renderer, cpus=cpus, workers=workers, nice=os.getpriority(os.PRIO_PROCESS,0),
                caps={k:env[k] for k in ("CARGO_BUILD_JOBS","RAYON_NUM_THREADS","RUST_TEST_THREADS","RUSTPY_BACKGROUND_THREADS")},
                memory_limit_bytes=args.memory_limit_bytes, peak_tree_rss_bytes=0, peak_app_rss_bytes=0, peak_app_tree_rss_bytes=0,
                app_pid=None, app_pid_verified=False, timed_out=False)


def run_million(args, output, env, cpus, workers):
    metadata = base_metadata(args, env, cpus, workers)
    metadata["limit_seconds"] = args.seconds
    if args.test_binary is not None:
        if not args.test_binary.is_file():
            raise ValueError("--test-binary does not exist")
        executable = args.test_binary.resolve()
        metadata["build"] = "prebuilt --test-binary"
    else:
        build_command, executable = build_million(output, env)
        metadata["build_command"] = build_command
    metadata["test_binary_sha256"] = hashlib.sha256(executable.read_bytes()).hexdigest()
    command = [str(executable), MILLION_TEST, "--ignored", "--exact", "--nocapture", "--test-threads=1"]
    env = dict(env, RAYON_NUM_THREADS=str(workers))
    supervise(command, output, env, metadata, args.seconds, args.memory_limit_bytes)
    raw = (output/"stdout.log").read_text(errors="replace") + (output/"stderr.log").read_text(errors="replace")
    result = million_result(raw, metadata["returncode"], metadata)
    (output/"run.json").write_text(json.dumps(metadata,indent=2))
    return result


def run_native(args, output, env, cpus, workers, profile):
    delay, deadline = native_contract(args.seconds,args.renderer)
    if args.capture_after is not None:
        delay = args.capture_after
    requested = args.release_binary if profile == "release" else args.binary
    binary, binary_sha = measured_binary(requested, smoke_binary(profile))
    metadata = base_metadata(args, env, cpus, workers)
    env = dict(env)
    project, active, script, inventory, marks = prepare(output,args.stage,args.step_scale)
    metadata.update(inventory=inventory, script_steps=marks, step_scale=args.step_scale)
    for key, directory in (("RUSTPY_CONFIG_DIR","config"),("RUSTPY_SYSTEM_DIR","system"),("RUSTPY_LOG_DIR","logs")):
        (output/directory).mkdir()
        env[key] = str(output/directory)
    # Trust only the generated canonical fixture, using the IDE-owned
    # settings authority. No user project or containing location is trusted.
    settings = {"trusted_projects":[str(project.resolve())],
                "updates":{"check_automatically":False,"download_automatically":False},
                "diagnostics":{"collect_usage_statistics":False}}
    (output/"config"/"settings.json").write_text(json.dumps(settings,indent=2))
    env["RUSTPY_PERFORMANCE_TRACE"] = str(output/"trace.jsonl")
    env["RUSTPY_PERFORMANCE_TRACE_MAX_ROWS"] = str(args.trace_max_rows)
    env["RUSTPY_PERFORMANCE_TRACE_COUNT_ONLY"] = args.trace_count_only
    env = smoke_environment(env,delay,profile,args.seconds,deadline)
    command = smoke_command(project,active,script,output)
    metadata.update(binary_sha256=binary_sha, binary_path=str(binary), profile=profile,
                    roadmap_target_profile=ROADMAP_TARGET_PROFILE,
                    smoke_script_sha256=hashlib.sha256(SMOKE.read_bytes()).hexdigest(),
                    gui_route="scripts/smoke.sh", native_timeout_seconds=args.seconds,
                    screenshot_delay_seconds=delay, screenshot_deadline_seconds=deadline,
                    trace_max_rows=args.trace_max_rows,
                    trace_count_only=[k for k in args.trace_count_only.split(",") if k.strip()],
                    hardware_measurement="unavailable: smoke.sh forces software renderer")
    # The wrapper's own timeout ends the run; the driver's watchdog is a backstop.
    supervise(command, output, env, metadata, args.seconds+10, args.memory_limit_bytes, project, binary_sha)
    trace = output/"trace.jsonl"
    try: rows = [json.loads(line) for line in trace.read_text().splitlines()]
    except (OSError,ValueError) as error: rows = []; metadata["trace_error"] = str(error)
    result = evaluate(rows,metadata)
    result.update(profile=profile, applies_to_roadmap_target=profile == ROADMAP_TARGET_PROFILE)
    (output/"run.json").write_text(json.dumps(metadata,indent=2))
    (output/"acceptance.json").write_text(json.dumps(result,indent=2))
    return result


def supervise(command, output, env, metadata, seconds, memory_limit, project=None, binary_sha=None):
    """Runs `command` under the whole-process-tree memory and time watchdog."""
    native = project is not None
    metadata["command"] = command
    metadata["start_epoch_ns"] = time.time_ns()
    with (output/"stdout.log").open("wb") as stdout, (output/"stderr.log").open("wb") as stderr:
        process = subprocess.Popen(command,cwd=ROOT,env=env,stdout=stdout,stderr=stderr,start_new_session=True)
        metadata["wrapper_pid"] = process.pid
        started = time.monotonic()
        sentinel_written = False
        try:
            while process.poll() is None:
                elapsed = time.monotonic()-started
                parents, resident = proc_snapshot()
                metadata["peak_tree_rss_bytes"] = max(metadata["peak_tree_rss_bytes"],process_tree_rss(process.pid,parents,resident))
                if native:
                    if not metadata["app_pid_verified"]:
                        pid = trace_pid(output/"trace.jsonl")
                        if pid is not None:
                            try:
                                metadata.update(identify_app(pid,process.pid,parents,binary_sha))
                            except (OSError,ValueError) as error:
                                metadata["app_identity_error"] = str(error)
                                terminate(process)
                                break
                    if metadata["app_pid_verified"]:
                        app_pid = metadata["app_pid"]
                        metadata["peak_app_rss_bytes"] = max(metadata["peak_app_rss_bytes"],resident.get(app_pid,0))
                        metadata["peak_app_tree_rss_bytes"] = max(metadata["peak_app_tree_rss_bytes"],process_tree_rss(app_pid,parents,resident))
                if metadata["peak_tree_rss_bytes"] >= memory_limit:
                    # Cooperative shared-PC watchdog: preserve failure rather
                    # than continuing an already failed memory/resource gate.
                    metadata["memory_limit_exceeded"] = True
                    terminate(process)
                    break
                if native and elapsed > 8 and not sentinel_written:
                    sentinel = project/"external-watcher-sentinel.py"
                    metadata["watcher_path"] = str(sentinel)
                    metadata["watcher_write_epoch_ns"] = time.time_ns()
                    sentinel.write_text("external_change = True\n")
                    sentinel_written = True
                if elapsed > seconds:
                    metadata["timed_out"] = True
                    terminate(process)
                    break
                time.sleep(.05)
        finally:
            terminate(process)
        metadata["returncode"] = process.returncode
        metadata["elapsed_seconds"] = time.monotonic()-started


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("stage",choices=("native10k","memory100k","largefile","million"))
    parser.add_argument("--binary",type=Path,help="centrally built debug binary (must match ./target/debug/rustpy)")
    parser.add_argument("--release-binary",type=Path,help="centrally built release binary (must match ./target/release/rustpy)")
    parser.add_argument("--profile",choices=("debug","release","both"),default="debug",help=f"native build profile(s) measured; roadmap targets apply to {ROADMAP_TARGET_PROFILE}")
    parser.add_argument("--output",type=Path,required=True)
    parser.add_argument("--renderer",choices=("gpu","software"),default="software")
    parser.add_argument("--seconds",type=int,help="native run budget (60..900, default 90) or million test limit (30..3600, default 300)")
    parser.add_argument("--cores",type=int,default=DEFAULT_CORES,help="CPU affinity cap for the driver and everything it starts")
    parser.add_argument("--workers",type=int,help="background analysis workers (RUSTPY_BACKGROUND_THREADS); default cores-1")
    parser.add_argument("--memory-limit-bytes",type=int,default=1_000_000_000,help="whole-process-tree safety watchdog")
    parser.add_argument("--test-binary",type=Path,help="million: prebuilt million_line_project test binary (skips the build)")
    parser.add_argument("--step-scale",type=float,default=1.0,help="multiplies every wait in the native input script")
    parser.add_argument("--capture-after",type=float,help="minimum seconds before the native capture (default min(seconds-20, 90))")
    parser.add_argument("--trace-max-rows",type=int,default=1_000_000)
    parser.add_argument("--trace-count-only",default="frame",help="comma-separated trace kinds only counted, not written")
    parser.add_argument("--execute",action="store_true")
    args = parser.parse_args()
    try:
        if args.stage == "million":
            args.seconds = 300 if args.seconds is None else args.seconds
            if not 30 <= args.seconds <= 3600: parser.error("million timeout must be 30..3600 seconds")
            if args.profile != "debug": parser.error("--profile applies to native stages; million always runs the release test")
        else:
            args.seconds = 90 if args.seconds is None else args.seconds
            native_contract(args.seconds,args.renderer)
        cores, workers = worker_plan(args.cores, args.workers)
        if args.step_scale <= 0 or args.trace_max_rows < 2: raise ValueError("--step-scale must be positive and --trace-max-rows at least 2")
        if args.capture_after is not None and not 0 <= args.capture_after <= 90: raise ValueError("--capture-after must be 0..90")
    except ValueError as error: parser.error(str(error))
    if not args.execute:
        print(json.dumps(dict(stage=args.stage, output=str(args.output), renderer=args.renderer, profile=args.profile,
                              roadmap_target_profile=ROADMAP_TARGET_PROFILE, seconds=args.seconds, cores=cores, workers=workers,
                              execution="deferred; add --execute only during final verification",
                              limits=f"{cores} cores, nice>=10, Cargo/Rayon/test {cores}, {workers} background workers, existing shared target and nested TMPDIR",
                              evidence=["run.json","inventory.json (native)","trace.jsonl (native)","acceptance.json","stdout.log","stderr.log","build.jsonl/build.log (million)","final.png (native)"]),indent=2))
        return 0
    try: return run(args)
    except (OSError,ValueError) as error:
        print(f"unavailable/error (not passed): {error}",file=sys.stderr)
        return 2

if __name__ == "__main__":
    sys.exit(main())

# Final additional source-contract commands (same inherited resource controls):
# python3 -m unittest discover -s qa/performance -p 'test_*.py'
# cargo test --locked --release -p rustpy-ui -p rustpy-index -p rustpy-analysis --lib
# Run the native10k driver five times into distinct empty nested output dirs for
# cold-start evidence (--profile both), then memory100k and largefile, all
# through smoke.sh. Hardware/native platforms remain unavailable through this
# software-only path; no whole-roadmap acceptance is inferred from these stages.
