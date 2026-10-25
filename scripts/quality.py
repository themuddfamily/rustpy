#!/usr/bin/env python3
"""Print final gates by default; execute one explicit gate with Linux CPU caps.
No dependency installation, downloads, GUI startup or publication is implicit.
"""
import argparse
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import shutil

ROOT = Path(__file__).resolve().parent.parent
STAGES = {
    "fmt": ["cargo", "fmt", "--all", "--", "--check"],
    "clippy": ["cargo", "clippy", "--locked", "--workspace", "--all-targets", "--", "-D", "warnings"],
    "unit": ["cargo", "test", "--locked", "--workspace", "--no-fail-fast", "--", "--test-threads=2"],
    "docs": ["cargo", "test", "--locked", "--workspace", "--doc", "--", "--test-threads=2"],
    "build": ["cargo", "build", "--locked", "-p", "rustpy", "--bin", "rustpy"],
    "qa": ["cargo", "test", "--locked", "--manifest-path", "qa/Cargo.toml", "--", "--test-threads=2"],
    "oracle-valid": ["cargo", "test", "--locked", "-p", "rustpy-syntax", "--test", "corpus", "corpus_stdlib_parses_without_errors", "--", "--ignored", "--test-threads=1"],
    "oracle-mutation": ["cargo", "test", "--locked", "-p", "rustpy-syntax", "--test", "corpus", "corpus_mutated_sources_never_panic", "--", "--ignored", "--test-threads=1"],
    "corpus": ["cargo", "test", "--locked", "--manifest-path", "qa/Cargo.toml", "--test", "parser_sources", "--", "--ignored", "--test-threads=1"],
    "deny": ["cargo", "deny", "--frozen", "--config", "qa/deny.toml", "check"],
    "bench": ["cargo", "bench", "--locked", "--manifest-path", "qa/Cargo.toml", "--bench", "editor_pipeline"],
    "fuzz": ["cargo", "+nightly", "fuzz", "run", "--fuzz-dir", "qa/fuzz", "python", "--", "-max_total_time=60", "-max_len=65536", "-rss_limit_mb=2048", "-jobs=1", "-workers=1"],
}

SERIAL_UNIT_TEST = (
    "app::analysis_jobs::typing_latency_tests::"
    "an_edit_cancels_the_obsolete_analysis_of_a_five_thousand_line_file_promptly"
)
SERIAL_UNIT_TESTS = (SERIAL_UNIT_TEST,
    "app::analysis_jobs::typing_latency_tests::typing_into_a_five_thousand_line_file_keeps_ui_frames_short")


def stage_commands(stage):
    """Keep the strict latency case required, without concurrent test bodies."""
    command = list(STAGES[stage])
    if stage != "unit":
        return [command]
    cargo_arguments = command[:command.index("--")]
    for test in SERIAL_UNIT_TESTS:
        command += ["--skip", test]
    return [command, *[cargo_arguments + [test, "--", "--exact", "--test-threads=1"]
                      for test in SERIAL_UNIT_TESTS]]


def execute_commands(commands, environment):
    """Run every required phase and retain the first failing exit status."""
    status = 0
    for command in commands:
        print(shlex.join(command), flush=True)
        try:
            result = subprocess.run(command, env=environment, check=False)
            code = result.returncode
            if code < 0:
                code = 128 - code
        except OSError as error:
            print(f"Cannot run {command[0]}: {error}", file=sys.stderr, flush=True)
            code = 127
        if code and not status:
            status = code
    return status


def output_root(value):
    """Directory that receives the per-gate output directory: RUSTPY_QUALITY_DIR
    when set (it must exist), otherwise the system temporary directory."""
    if not value:
        return Path(tempfile.gettempdir())
    path = Path(value).resolve()
    if not path.is_dir():
        raise ValueError(f"RUSTPY_QUALITY_DIR must be an existing directory: {path}")
    return path


def admitted_corpus(value, extensions=(".py", ".pyi")):
    if not value:
        raise ValueError("explicit nonempty corpus roots required")
    roots = value.split(os.pathsep)
    for raw in roots:
        if not raw:
            raise ValueError("empty corpus path component is not a declared root")
        root = Path(raw)
        if not root.is_dir() or root.is_symlink():
            raise ValueError(f"corpus root must be an existing non-symlink directory: {root}")
        # Admission must not silently delegate an empty root to a permissive
        # upstream test. Do not follow symlinks or admit FIFOs/devices.
        found = False
        for base, directories, files in os.walk(root,followlinks=False):
            directories[:] = [name for name in directories if not (Path(base)/name).is_symlink()]
            if any((Path(base)/name).suffix in extensions and not (Path(base)/name).is_symlink() and (Path(base)/name).is_file() for name in files):
                found = True
                break
        if not found:
            raise ValueError(f"declared corpus root has no regular {', '.join(extensions)} inputs: {root}")
    return roots


def gate_environment(environment, gate_directory):
    env = environment.copy()
    for key in ("CARGO_BUILD_JOBS", "RAYON_NUM_THREADS", "RUST_TEST_THREADS"):
        env[key] = "2"
    env["CARGO_NET_OFFLINE"] = "true"
    env["RUSTUP_AUTO_INSTALL"] = "0"
    env.pop("RUSTPY_TRUST_ALL",None)
    env["CARGO_TARGET_DIR"] = str(Path(env.get("CARGO_TARGET_DIR") or ROOT/"target").resolve())
    for key, name in (("RUSTPY_CONFIG_DIR","config"),("RUSTPY_SYSTEM_DIR","system"),("RUSTPY_LOG_DIR","logs"),("XDG_CONFIG_HOME","xdg-config"),("XDG_CACHE_HOME","xdg-cache"),("XDG_DATA_HOME","xdg-data"),("XDG_STATE_HOME","xdg-state")):
        directory = gate_directory/name
        directory.mkdir()
        env[key] = str(directory)
    env["TMPDIR"] = str(gate_directory)
    return env


def fuzz_arguments(command, gate_directory):
    corpus = gate_directory/"fuzz-corpus"
    corpus.mkdir()
    artifacts = gate_directory/"fuzz-artifacts"
    artifacts.mkdir()
    # libFuzzer writes only its first corpus; tracked seeds are a secondary
    # read-only input. Never grow or minimize the repository seed directory.
    command = list(command)
    at = command.index("--") + 1
    command[at:at] = [str(corpus), str(ROOT/"qa/fuzz/corpus/python")]
    command.append("-artifact_prefix=" + str(artifacts) + os.sep)
    return command


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("stage", choices=STAGES)
    parser.add_argument("--execute", action="store_true", help="execute the selected final gate; otherwise print only")
    args = parser.parse_args()
    commands = stage_commands(args.stage)
    for command in commands:
        print(shlex.join(command), flush=True)
    if not args.execute:
        return 0
    if not hasattr(os, "sched_getaffinity") or not hasattr(os, "sched_setaffinity"):
        parser.error("execution wrapper requires Linux affinity; use documented native-platform resource controls")
    try:
        temporary = output_root(os.environ.get("RUSTPY_QUALITY_DIR"))
        if args.stage == "corpus":
            admitted_corpus(os.environ.get("RUSTPY_VALID_CORPUS_DIRS"))
        if args.stage.startswith("oracle-"):
            admitted_corpus(os.environ.get("RUSTPY_CORPUS_DIRS"), (".py",))
            python = os.environ.get("RUSTPY_CORPUS_PYTHON")
            if not python or shutil.which(python) is None:
                raise ValueError("set RUSTPY_CORPUS_PYTHON to an existing explicitly provisioned interpreter")
        if args.stage == "fuzz":
            admitted_corpus(str(ROOT/"qa/fuzz/corpus/python"))
    except ValueError as error:
        parser.error(str(error))
    cpus = sorted(os.sched_getaffinity(0))[:2]
    os.sched_setaffinity(0, cpus)
    if os.getpriority(os.PRIO_PROCESS,0) < 10:
        os.nice(10-os.getpriority(os.PRIO_PROCESS,0))
    # Preserve failed gate output in a fresh directory; Cargo keeps using the
    # normal (or CARGO_TARGET_DIR) target directory and its build cache.
    gate_directory = Path(tempfile.mkdtemp(prefix="rustpy-quality-" + args.stage + "-",dir=temporary))
    environment = gate_environment(os.environ,gate_directory)
    if args.stage == "fuzz":
        commands = [fuzz_arguments(commands[0],gate_directory)]
    print("isolated outputs: " + str(gate_directory),flush=True)
    os.chdir(ROOT)
    if len(commands) > 1:
        return execute_commands(commands, environment)
    command = commands[0]
    print(shlex.join(command),flush=True)
    os.execvpe(command[0], command, environment)


if __name__ == "__main__":
    sys.exit(main())
