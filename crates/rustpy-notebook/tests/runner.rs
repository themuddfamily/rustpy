//! End-to-end cell execution against the system `python3` (each test is skipped when it is missing).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustpy_notebook::{
    Cell, CellRunner, CellStatus, ConsoleEvent, KernelState, MimeBundle, Notebook, Output,
};

const TIMEOUT: Duration = Duration::from_secs(30);

fn python() -> Option<PathBuf> {
    let python = which::which("python3").ok();
    if python.is_none() {
        eprintln!("python3 not found; skipping");
    }
    python
}

fn start(python: &Path, dir: &tempfile::TempDir) -> CellRunner {
    CellRunner::start(python, dir.path(), &[], "").expect("kernel starts")
}

fn notebook(sources: &[&str]) -> Notebook {
    let mut nb = Notebook::empty();
    for source in sources {
        let end = nb.cells.len();
        nb.insert_cell(end, Cell::code(*source));
    }
    nb
}

fn plain(text: &str) -> MimeBundle {
    MimeBundle::from([("text/plain".to_owned(), text.to_owned())])
}

fn wait(runner: &mut CellRunner, nb: &mut Notebook) {
    assert!(
        runner.wait_idle(nb, TIMEOUT),
        "cells did not finish: {:#?}",
        nb.cells
    );
}

/// Polls until `done` holds, collecting the changed cell indices reported on the way.
fn poll_until(
    runner: &mut CellRunner,
    nb: &mut Notebook,
    mut done: impl FnMut(&CellRunner, &Notebook) -> bool,
) -> Vec<usize> {
    let deadline = Instant::now() + TIMEOUT;
    let mut changed = Vec::new();
    while !done(runner, nb) {
        assert!(Instant::now() < deadline, "timed out: {:#?}", nb.cells);
        changed.extend(runner.poll(nb));
        std::thread::sleep(Duration::from_millis(5));
    }
    changed
}

fn error_of(output: &Output) -> (&str, &str, &[String]) {
    match output {
        Output::Error {
            ename,
            evalue,
            traceback,
            ..
        } => (ename, evalue, traceback),
        other => panic!("expected an error output, got {other:?}"),
    }
}

#[test]
fn cells_queued_during_startup_wait_for_the_ready_event() {
    let Some(py) = python() else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut runner = start(&py, &dir);
    let mut nb = notebook(&["21", "2 * 21"]);
    // Ready is admitted by poll(), even if the child has already emitted it.
    assert_eq!(runner.state(), KernelState::Starting);
    assert_eq!(runner.run_all(&mut nb), [0, 1]);
    assert_eq!(runner.state(), KernelState::Starting);
    assert!(runner.is_busy());
    assert_eq!(runner.running_cell(&nb), None);
    assert_eq!(runner.execution_count(), 0);
    for cell in &nb.cells {
        assert_eq!(runner.cell_status(cell), CellStatus::Queued);
        assert!(cell.outputs.is_empty());
    }
    wait(&mut runner, &mut nb);
    assert_eq!(runner.state(), KernelState::Idle);
    assert_eq!(nb.cells[0].execution_count, Some(1));
    assert_eq!(nb.cells[1].execution_count, Some(2));
    assert_eq!(
        nb.cells[0].outputs,
        [Output::execute_result(Some(1), plain("21"))]
    );
    assert_eq!(
        nb.cells[1].outputs,
        [Output::execute_result(Some(2), plain("42"))]
    );
}

#[test]
fn runs_two_code_cells_in_order() {
    let Some(py) = python() else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut runner = start(&py, &dir);
    let mut nb = notebook(&["x = 21\nprint('hello', x)", "x * 2"]);

    poll_until(&mut runner, &mut nb, |runner, _| {
        matches!(runner.state(), KernelState::Idle | KernelState::Dead)
    });
    assert_eq!(runner.state(), KernelState::Idle, "kernel readiness failed");
    assert_eq!(runner.run_all(&mut nb), [0, 1]);
    assert!(runner.is_busy());
    assert_eq!(runner.cell_status(&nb.cells[0]), CellStatus::Running);
    assert_eq!(runner.cell_status(&nb.cells[1]), CellStatus::Queued);
    assert_eq!(runner.running_cell(&nb), Some(0));
    let changed = poll_until(&mut runner, &mut nb, |r, _| !r.is_busy());

    assert_eq!(
        nb.cells[0].outputs,
        [Output::stream("stdout", "hello 21\n")]
    );
    assert_eq!(nb.cells[0].execution_count, Some(1));
    assert_eq!(
        nb.cells[1].outputs,
        [Output::execute_result(Some(2), plain("42"))]
    );
    assert_eq!(nb.cells[1].execution_count, Some(2));
    assert!(changed.contains(&0) && changed.contains(&1));
    assert_eq!(runner.execution_count(), 2);
    assert_eq!(runner.state(), KernelState::Idle);
    assert_eq!(runner.cell_status(&nb.cells[1]), CellStatus::Idle);
    assert!(runner.cell_duration(&nb.cells[1]).is_some());
    assert!(runner.kernel_version().is_some_and(|v| v.starts_with('3')));
    assert!(runner.kernel_executable().is_some());

    // Re-running a cell clears its outputs, gives it the next count and only reports that cell.
    assert!(runner.run_cell(&mut nb, 1));
    assert!(nb.cells[1].outputs.is_empty() && nb.cells[1].execution_count.is_none());
    let changed = poll_until(&mut runner, &mut nb, |r, _| !r.is_busy());
    assert_eq!(
        nb.cells[1].outputs,
        [Output::execute_result(Some(3), plain("42"))]
    );
    assert_eq!(nb.cells[1].execution_count, Some(3));
    assert!(changed.iter().all(|&i| i == 1), "changed: {changed:?}");
    assert!(runner.poll(&mut nb).is_empty());

    // Outputs survive saving.
    let path = dir.path().join("run.ipynb");
    nb.save(&path).unwrap();
    assert_eq!(Notebook::load(&path).unwrap(), nb);
}

#[test]
fn an_error_stops_the_queue() {
    let Some(py) = python() else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut runner = start(&py, &dir);
    let mut nb = notebook(&[
        "print('before')\ndef g():\n    return 1/0\ng()",
        "print('never')",
        "y = 1",
    ]);
    runner.run_all(&mut nb);
    wait(&mut runner, &mut nb);

    let cell = &nb.cells[0];
    assert_eq!(cell.execution_count, Some(1));
    assert_eq!(cell.outputs.len(), 2, "{:?}", cell.outputs);
    assert_eq!(cell.outputs[0], Output::stream("stdout", "before\n"));
    let (ename, evalue, traceback) = error_of(&cell.outputs[1]);
    assert_eq!((ename, evalue), ("ZeroDivisionError", "division by zero"));
    let text = traceback.join("\n");
    assert!(
        text.starts_with("Traceback (most recent call last):"),
        "{text}"
    );
    assert!(
        text.contains("File \"<cell-1>\", line 4, in <module>") && text.contains("return 1/0"),
        "{text}"
    );
    assert!(!text.contains("rustpy"), "helper frames leaked: {text}");
    for cell in &nb.cells[1..] {
        assert!(cell.outputs.is_empty() && cell.execution_count.is_none());
        assert_eq!(runner.cell_status(cell), CellStatus::Idle);
    }

    // The kernel is still usable and keeps counting.
    assert!(runner.run_cell(&mut nb, 1));
    wait(&mut runner, &mut nb);
    assert_eq!(nb.cells[1].outputs, [Output::stream("stdout", "never\n")]);
    assert_eq!(nb.cells[1].execution_count, Some(2));

    // Syntax errors are reported without running anything.
    nb.cells[2].source = "print('ran')\nx = = 1".into();
    runner.run_cell(&mut nb, 2);
    wait(&mut runner, &mut nb);
    let (ename, _, traceback) = error_of(&nb.cells[2].outputs[0]);
    assert_eq!(ename, "SyntaxError");
    assert!(
        traceback.iter().any(|l| l.contains("x = = 1")),
        "{traceback:?}"
    );
    assert_eq!(nb.cells[2].outputs.len(), 1);
    assert_eq!(nb.cells[2].execution_count, Some(3));
}

#[test]
fn cells_follow_notebook_semantics() {
    let Some(py) = python() else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut runner = start(&py, &dir);
    let mut nb = notebook(&[
        // A compound statement needs no trailing blank line, and a statement may follow a block.
        "def f():\n    return 3",
        "class A:\n    def __repr__(self):\n        return 'line1\\nline2'\nA()",
        "f();",
        "import sys\nprint('out')\nprint('err', file=sys.stderr)\nprint('out2')",
        "class H:\n    def _repr_html_(self):\n        return '<b>hi</b>'\n    def __repr__(self):\n        return 'H()'\ndisplay(H(), 5)\n'done'",
        "print('10%', end='\\r')\nprint('100%')\nf() + _.__len__()",
        "import sys\nsys.exit(4)",
    ]);
    runner.run_all(&mut nb);
    wait(&mut runner, &mut nb);

    let outputs: Vec<&[Output]> = nb.cells.iter().map(|c| c.outputs.as_slice()).collect();
    assert!(outputs[0].is_empty());
    assert_eq!(
        outputs[1],
        [Output::execute_result(Some(2), plain("line1\nline2"))]
    );
    assert!(
        outputs[2].is_empty(),
        "`;` hides the result: {:?}",
        outputs[2]
    );
    assert_eq!(
        outputs[3],
        [
            Output::stream("stdout", "out\n"),
            Output::stream("stderr", "err\n"),
            Output::stream("stdout", "out2\n")
        ]
    );
    let html = MimeBundle::from([
        ("text/html".to_owned(), "<b>hi</b>".to_owned()),
        ("text/plain".to_owned(), "H()".to_owned()),
    ]);
    assert_eq!(
        outputs[4],
        [
            Output::display_data(html),
            Output::display_data(plain("5")),
            Output::execute_result(Some(5), plain("'done'"))
        ]
    );
    // `\r` overwrites the line; `_` is the last displayed result ('done').
    assert_eq!(
        outputs[5],
        [
            Output::stream("stdout", "100%\n"),
            Output::execute_result(Some(6), plain("7"))
        ]
    );
    let (ename, evalue, _) = error_of(&outputs[6][0]);
    assert_eq!((ename, evalue), ("SystemExit", "4"));
    assert_eq!(
        runner.state(),
        KernelState::Idle,
        "sys.exit() does not stop the kernel"
    );
    let counts: Vec<Option<u32>> = nb.cells.iter().map(|c| c.execution_count).collect();
    assert_eq!(counts, (1..=7).map(Some).collect::<Vec<_>>());
}

#[cfg(unix)]
#[test]
fn interrupt_stops_the_running_cell_and_the_queue() {
    let Some(py) = python() else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut runner = start(&py, &dir);
    let mut nb = notebook(&[
        "import time\nprint('start', flush=True)\ntime.sleep(30)",
        "print('never')",
    ]);
    runner.run_all(&mut nb);
    poll_until(&mut runner, &mut nb, |_, nb| {
        !nb.cells[0].outputs.is_empty()
    });
    // Give the cell time to get from the print into the sleep, so the interrupt lands there.
    std::thread::sleep(Duration::from_millis(200));
    let started = Instant::now();
    runner.interrupt();
    assert_eq!(
        runner.cell_status(&nb.cells[1]),
        CellStatus::Idle,
        "queued cells are cancelled at once"
    );
    wait(&mut runner, &mut nb);
    assert!(started.elapsed() < Duration::from_secs(10));

    assert_eq!(nb.cells[0].outputs[0], Output::stream("stdout", "start\n"));
    let (ename, _, traceback) = error_of(&nb.cells[0].outputs[1]);
    assert_eq!(ename, "KeyboardInterrupt");
    assert!(
        traceback.iter().any(|l| l.contains("time.sleep(30)")),
        "{traceback:?}"
    );
    assert_eq!(nb.cells[0].execution_count, Some(1));
    assert!(nb.cells[1].outputs.is_empty() && nb.cells[1].execution_count.is_none());

    // The kernel survives the interrupt.
    assert_eq!(runner.state(), KernelState::Idle);
    runner.run_cell(&mut nb, 1);
    wait(&mut runner, &mut nb);
    assert_eq!(nb.cells[1].outputs, [Output::stream("stdout", "never\n")]);
}

#[test]
fn restart_clears_variables_and_counts() {
    let Some(py) = python() else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut runner = start(&py, &dir);
    let mut nb = notebook(&["x = 5\nx", "'x' in dir()"]);
    runner.run_all(&mut nb);
    wait(&mut runner, &mut nb);
    assert_eq!(
        nb.cells[1].outputs,
        [Output::execute_result(Some(2), plain("True"))]
    );

    runner.restart();
    assert_eq!(runner.execution_count(), 0);
    assert!(matches!(
        runner.state(),
        KernelState::Starting | KernelState::Idle
    ));
    runner.run_cell(&mut nb, 1);
    wait(&mut runner, &mut nb);
    assert_eq!(
        nb.cells[1].outputs,
        [Output::execute_result(Some(1), plain("False"))]
    );
    assert_eq!(nb.cells[1].execution_count, Some(1));
    // The first cell keeps the outputs of its run in the old kernel.
    assert_eq!(nb.cells[0].execution_count, Some(1));

    // Restarting while a cell runs stops it without a count.
    nb.cells[0].source = "import time\nprint('tick', flush=True)\ntime.sleep(30)".into();
    runner.run_all(&mut nb);
    poll_until(&mut runner, &mut nb, |_, nb| {
        !nb.cells[0].outputs.is_empty()
    });
    runner.restart();
    let changed = runner.poll(&mut nb);
    assert!(
        changed.contains(&0) && changed.contains(&1),
        "changed: {changed:?}"
    );
    assert!(!runner.is_busy());
    assert_eq!(nb.cells[0].outputs, [Output::stream("stdout", "tick\n")]);
    assert_eq!(
        (nb.cells[0].execution_count, nb.cells[1].execution_count),
        (None, None)
    );
}

#[test]
fn a_dead_kernel_is_reported_and_restarted_on_the_next_run() {
    let Some(py) = python() else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut runner = start(&py, &dir);
    let mut nb = notebook(&["import os\nos._exit(3)", "print('alive')"]);
    runner.run_all(&mut nb);
    wait(&mut runner, &mut nb);
    assert_eq!(runner.state(), KernelState::Dead);
    let (ename, evalue, _) = error_of(&nb.cells[0].outputs[0]);
    assert_eq!(ename, "DeadKernelError");
    assert!(evalue.contains("code 3"), "{evalue}");
    assert!(nb.cells[1].outputs.is_empty() && nb.cells[1].execution_count.is_none());

    assert!(runner.run_cell(&mut nb, 1));
    wait(&mut runner, &mut nb);
    assert_eq!(nb.cells[1].outputs, [Output::stream("stdout", "alive\n")]);
    assert_eq!(nb.cells[1].execution_count, Some(1));
    assert_ne!(runner.state(), KernelState::Dead);

    runner.shutdown();
    assert_eq!(runner.state(), KernelState::Dead);
}

#[test]
fn run_above_below_and_skipped_cells() {
    let Some(py) = python() else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut runner = start(&py, &dir);
    let mut nb = notebook(&["a = 1", "b = 2", "", "c = a + b\nc"]);
    nb.insert_cell(2, Cell::markdown("# Notes"));
    // [0] a = 1, [1] b = 2, [2] markdown, [3] empty code, [4] c = a + b
    nb.cells[3].outputs.push(Output::stream("stdout", "stale"));
    nb.cells[3].execution_count = Some(9);

    assert_eq!(runner.run_above(&mut nb, 2), [0, 1]);
    assert_eq!(runner.run_below(&mut nb, 2), [4]);
    assert!(
        nb.cells[3].outputs.is_empty() && nb.cells[3].execution_count.is_none(),
        "empty cells are cleared, not run"
    );
    assert!(!runner.run_cell(&mut nb, 2), "markdown cells do not run");
    assert!(!runner.run_cell(&mut nb, 99));
    wait(&mut runner, &mut nb);
    assert_eq!(
        nb.cells[4].outputs,
        [Output::execute_result(Some(3), plain("3"))]
    );
    assert!(nb.cells[2].outputs.is_empty());
    assert_eq!(runner.run_above(&mut nb, 0), Vec::<usize>::new());
    assert_eq!(runner.run_below(&mut nb, 5), Vec::<usize>::new());
}

#[test]
fn cells_are_tracked_by_id_while_the_notebook_changes() {
    let Some(py) = python() else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut runner = start(&py, &dir);
    let mut nb = notebook(&[
        "import time\ntime.sleep(0.3)\n'first'",
        "'second'",
        "'third'",
    ]);
    poll_until(&mut runner, &mut nb, |runner, _| {
        matches!(runner.state(), KernelState::Idle | KernelState::Dead)
    });
    assert_eq!(runner.state(), KernelState::Idle, "kernel readiness failed");
    runner.run_all(&mut nb);
    // Edit the notebook while the first cell runs.
    nb.delete_cell(1);
    nb.insert_cell(0, Cell::code("'inserted'"));
    nb.cells[2].source = "'edited after queueing'".into();
    nb.move_cell(1, 2);
    // Now: [0] inserted, [1] third (edited), [2] first (running)
    assert_eq!(runner.running_cell(&nb), Some(2));
    assert_eq!(runner.cell_status(&nb.cells[1]), CellStatus::Queued);
    wait(&mut runner, &mut nb);

    assert!(nb.cells[0].outputs.is_empty());
    assert_eq!(
        nb.cells[2].outputs,
        [Output::execute_result(Some(1), plain("'first'"))]
    );
    // The source at queueing time ran; the deleted cell was skipped without using a count.
    assert_eq!(
        nb.cells[1].outputs,
        [Output::execute_result(Some(2), plain("'third'"))]
    );
}

#[test]
fn startup_code_environment_and_kernel_requests() {
    let Some(py) = python() else { return };
    let dir = tempfile::tempdir().unwrap();
    let env = [("RUSTPY_NB_TEST".to_owned(), "42".to_owned())];
    // Startup code ending in an indented block runs too (the console's REPL rules do not apply).
    let mut runner = CellRunner::start(
        &py,
        dir.path(),
        &env,
        "import os\nif True:\n    FROM_STARTUP = os.environ['RUSTPY_NB_TEST']",
    )
    .unwrap();
    let mut nb = notebook(&[
        "FROM_STARTUP",
        "import os\nos.getcwd() == os.path.realpath(%r)",
        "values = [1, 2]",
    ]);
    nb.cells[1].source = nb.cells[1]
        .source
        .replace("%r", &format!("{:?}", dir.path().to_string_lossy()));
    runner.run_all(&mut nb);
    wait(&mut runner, &mut nb);
    assert_eq!(
        nb.cells[0].outputs,
        [Output::execute_result(Some(1), plain("'42'"))]
    );
    assert_eq!(
        nb.cells[1].outputs,
        [Output::execute_result(Some(2), plain("True"))]
    );

    let vars = runner.request_vars();
    let completions = runner.complete("valu", 4);
    let deadline = Instant::now() + TIMEOUT;
    let (mut got_vars, mut got_completions) = (None, None);
    while (got_vars.is_none() || got_completions.is_none()) && Instant::now() < deadline {
        runner.poll(&mut nb);
        for event in runner.take_events() {
            match event {
                ConsoleEvent::Vars { id, items } if id == vars => got_vars = Some(items),
                ConsoleEvent::Completions { id, items, .. } if id == completions => {
                    got_completions = Some(items)
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let vars = got_vars.expect("variables");
    assert!(
        vars.iter()
            .any(|v| v.name == "values" && v.type_name == "list"),
        "{vars:?}"
    );
    assert!(
        !vars.iter().any(|v| v.name.starts_with("__rustpy")),
        "the helper stays hidden: {vars:?}"
    );
    assert!(got_completions
        .expect("completions")
        .iter()
        .any(|c| c.name == "values"));
}

#[test]
fn output_outside_cells_is_reported_as_events() {
    let Some(py) = python() else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut runner = CellRunner::start(
        &py,
        dir.path(),
        &[],
        "print('from startup')\nraise ValueError('bad startup')",
    )
    .unwrap();
    let mut nb = notebook(&["1 + 1"]);
    let deadline = Instant::now() + TIMEOUT;
    let (mut out, mut err) = (String::new(), String::new());
    while !err.contains("ValueError: bad startup") {
        assert!(
            Instant::now() < deadline,
            "no startup error reported: {out:?} {err:?}"
        );
        assert!(runner.poll(&mut nb).is_empty(), "no cell changes");
        for event in runner.take_events() {
            match event {
                ConsoleEvent::Stdout(text) => out.push_str(&text),
                ConsoleEvent::Stderr(text) => err.push_str(&text),
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(out, "from startup\n");
    // The kernel works regardless: its helper is installed before the user's startup code runs.
    runner.run_all(&mut nb);
    wait(&mut runner, &mut nb);
    assert_eq!(
        nb.cells[0].outputs,
        [Output::execute_result(Some(1), plain("2"))]
    );
}

#[test]
fn late_output_of_background_threads_goes_to_the_cell_that_ran_last() {
    let Some(py) = python() else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut runner = start(&py, &dir);
    let mut nb = notebook(&["import threading, time\ndef later():\n    time.sleep(0.3)\n    print('late')\nthreading.Thread(target=later).start()\n'started'", "'second'"]);
    runner.run_cell(&mut nb, 0);
    let changed = poll_until(&mut runner, &mut nb, |r, nb| {
        !r.is_busy() && nb.cells[0].outputs.len() == 2
    });
    assert_eq!(
        nb.cells[0].outputs,
        [
            Output::execute_result(Some(1), plain("'started'")),
            Output::stream("stdout", "late\n")
        ]
    );
    assert!(changed.contains(&0));
    assert!(nb.cells[1].outputs.is_empty());
}

#[test]
fn magics_and_shell_commands() {
    let Some(py) = python() else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut runner = start(&py, &dir);
    let mut nb = notebook(&[
        "%matplotlib inline\nprint('ok')",
        "!echo from-shell",
        "s = '''\n%d items\n'''\nlen(s)",
        "%who",
    ]);
    runner.run_all(&mut nb);
    wait(&mut runner, &mut nb);
    assert_eq!(nb.cells[0].outputs, [Output::stream("stdout", "ok\n")]);
    assert_eq!(
        nb.cells[1].outputs,
        [Output::stream("stdout", "from-shell\n")]
    );
    // Valid Python is never rewritten, even when a line starts with `%`.
    assert_eq!(
        nb.cells[2].outputs,
        [Output::execute_result(Some(3), plain("10"))]
    );
    let (ename, evalue, _) = error_of(&nb.cells[3].outputs[0]);
    assert_eq!(ename, "UsageError");
    assert!(evalue.contains("%who"), "{evalue}");
}

/// Concatenated text of a cell's `name` stream outputs.
#[cfg(unix)]
fn stream_text(cell: &Cell, name: &str) -> String {
    cell.outputs
        .iter()
        .filter_map(|o| match o {
            Output::Stream { name: n, text, .. } if n == name => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[cfg(unix)]
#[test]
fn common_magics_are_emulated() {
    let Some(py) = python() else { return };
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let sub = std::fs::canonicalize(dir.path().join("sub"))
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut runner = start(&py, &dir);
    let mut nb = notebook(&[
        // The usual notebook preamble must not stop "Run All".
        "%config InlineBackend.figure_format = 'retina'\n%load_ext autoreload\n%autoreload 2\nprint('ready')",
        "%cd sub\n%pwd",
        "%env RUSTPY_NB_MAGIC=hello\n%env RUSTPY_NB_MAGIC",
        "%time y = 6 * 7\n%time y + 1",
        "\n%%time\nz = y\nz",
        "%%writefile notes.txt\nline 1\nline 2\n",
        "%%writefile -a notes.txt\nline 3\n",
        "%%bash\necho out\necho err >&2",
        "%%html\n<b>bold</b>",
    ]);
    runner.run_all(&mut nb);
    wait(&mut runner, &mut nb);
    let cells = &nb.cells;

    assert_eq!(stream_text(&cells[0], "stdout"), "ready\n");
    assert!(
        stream_text(&cells[0], "stderr").contains("autoreload is not available"),
        "{:?}",
        cells[0].outputs
    );
    assert_eq!(
        cells[1].outputs,
        [
            Output::stream("stdout", format!("{sub}\n")),
            Output::execute_result(Some(2), plain(&format!("'{sub}'")))
        ]
    );
    assert_eq!(
        cells[2].outputs,
        [
            Output::stream("stdout", "env: RUSTPY_NB_MAGIC=hello\n"),
            Output::execute_result(Some(3), plain("'hello'"))
        ]
    );
    let timing = stream_text(&cells[3], "stdout");
    assert_eq!(
        (
            timing.matches("CPU times: user ").count(),
            timing.matches("Wall time: ").count()
        ),
        (2, 2),
        "{timing}"
    );
    assert_eq!(
        cells[3].outputs.last(),
        Some(&Output::execute_result(Some(4), plain("43")))
    );
    assert!(
        stream_text(&cells[4], "stdout").contains("Wall time: "),
        "{:?}",
        cells[4].outputs
    );
    assert_eq!(
        cells[4].outputs.last(),
        Some(&Output::execute_result(Some(5), plain("42")))
    );
    assert_eq!(
        cells[5].outputs,
        [Output::stream("stdout", "Writing notes.txt\n")]
    );
    assert_eq!(
        cells[6].outputs,
        [Output::stream("stdout", "Appending to notes.txt\n")]
    );
    // `%cd` moved the kernel into `sub`.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("sub/notes.txt")).unwrap(),
        "line 1\nline 2\nline 3\n"
    );
    assert_eq!(cells[7].outputs, [Output::stream("stdout", "out\nerr\n")]);
    assert_eq!(
        cells[8].outputs,
        [Output::display_data(MimeBundle::from([(
            "text/html".to_owned(),
            "<b>bold</b>".to_owned()
        )]))]
    );

    // Failures are cell errors.
    let mut run_alone = |source: &str| {
        nb.cells[0].source = source.to_owned();
        assert!(runner.run_cell(&mut nb, 0));
        wait(&mut runner, &mut nb);
        assert_eq!(nb.cells[0].outputs.len(), 1, "{:?}", nb.cells[0].outputs);
        let (ename, evalue, traceback) = error_of(&nb.cells[0].outputs[0]);
        (ename.to_owned(), evalue.to_owned(), traceback.join("\n"))
    };
    let (ename, evalue, _) = run_alone("%%bash\nexit 3");
    assert_eq!(ename, "CalledProcessError");
    assert!(evalue.contains("exit status 3"), "{evalue}");
    let (ename, evalue, _) = run_alone("%%capture\nprint(1)");
    assert_eq!(ename, "UsageError");
    assert!(evalue.contains("%%capture"), "{evalue}");
    // A syntax error next to a magic is reported where it is, not at the magic.
    let (ename, evalue, traceback) = run_alone("%matplotlib inline\nx = = 1");
    assert_eq!(ename, "SyntaxError");
    assert!(
        evalue.contains("line 2")
            && traceback.contains("x = = 1")
            && !traceback.contains("During handling"),
        "{evalue}\n{traceback}"
    );
}

#[test]
fn objects_defined_in_cells_can_be_pickled() {
    let Some(py) = python() else { return };
    let dir = tempfile::tempdir().unwrap();
    let mut runner = start(&py, &dir);
    let mut nb = notebook(&[
        "import pickle\nclass Point:\n    def __init__(self, x):\n        self.x = x\ndef double(v):\n    return 2 * v",
        "copy = pickle.loads(pickle.dumps(Point(21)))\n(type(copy) is Point, pickle.loads(pickle.dumps(double))(copy.x))",
        // `__main__` is the user namespace, both ways.
        "import __main__\n__main__.injected = 'via __main__'\n(injected, 'Point' in dir(__main__), __main__.__name__)",
    ]);
    runner.run_all(&mut nb);
    wait(&mut runner, &mut nb);
    assert_eq!(
        nb.cells[1].outputs,
        [Output::execute_result(Some(2), plain("(True, 42)"))]
    );
    assert_eq!(
        nb.cells[2].outputs,
        [Output::execute_result(
            Some(3),
            plain("('via __main__', True, '__main__')")
        )]
    );

    // Worker processes find functions defined in cells (fork start method).
    if cfg!(target_os = "linux") {
        nb.cells[2].source = "import multiprocessing\nwith multiprocessing.get_context('fork').Pool(2) as pool:\n    result = pool.map(double, [1, 2, 3])\nresult".into();
        runner.run_cell(&mut nb, 2);
        wait(&mut runner, &mut nb);
        assert_eq!(
            nb.cells[2].outputs,
            [Output::execute_result(Some(4), plain("[2, 4, 6]"))]
        );
    }
}

#[test]
fn matplotlib_figures_become_png_outputs() {
    let Some(py) = python() else { return };
    let has_matplotlib = std::process::Command::new(&py)
        .args(["-c", "import matplotlib"])
        .output()
        .is_ok_and(|o| o.status.success());
    if !has_matplotlib {
        eprintln!("matplotlib not installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // MPLBACKEND is only defaulted when unset; make the test independent of the caller's environment.
    let env = [("MPLBACKEND".to_owned(), "Agg".to_owned())];
    let mut runner = CellRunner::start(&py, dir.path(), &env, "").unwrap();
    let mut nb = notebook(&[
        "import matplotlib.pyplot as plt\nplt.plot([1, 2, 3])\nplt.show()\nprint('plotted')",
        "fig = plt.figure()\nfig",
    ]);
    runner.run_all(&mut nb);
    wait(&mut runner, &mut nb);

    // (stderr may carry matplotlib's one-time "building the font cache" notice)
    let visible = |cell: &Cell| {
        cell.outputs
            .iter()
            .filter(|o| !matches!(o, Output::Stream { name, .. } if name == "stderr"))
            .cloned()
            .collect::<Vec<_>>()
    };
    let outputs = visible(&nb.cells[0]);
    assert_eq!(outputs.len(), 2, "{outputs:?}");
    assert_eq!(outputs[0], Output::stream("stdout", "plotted\n"));
    let Output::DisplayData { data, .. } = &outputs[1] else {
        panic!("expected a figure, got {:?}", outputs[1])
    };
    assert!(data["image/png"].starts_with("iVBORw0KGgo"));
    assert!(data["text/plain"].starts_with("<Figure size"));
    // A figure as the cell's value is shown once, as its result.
    let outputs = visible(&nb.cells[1]);
    assert_eq!(outputs.len(), 1, "{outputs:?}");
    assert!(
        matches!(&outputs[0], Output::ExecuteResult { data, .. } if data.contains_key("image/png"))
    );
}

#[test]
fn a_missing_interpreter_fails_to_start() {
    let dir = tempfile::tempdir().unwrap();
    assert!(CellRunner::start(
        &PathBuf::from("/nonexistent/python-xyz"),
        dir.path(),
        &[],
        ""
    )
    .is_err());
}
