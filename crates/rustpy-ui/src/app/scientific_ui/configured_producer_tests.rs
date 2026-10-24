//! Genuine scientific producers, followed by the existing App publication routes.
//! Run alone with an explicitly admitted SDK; no fixture installs dependencies.
use super::*;
use rustpy_notebook::{Cell, CellRunner, Connection, KernelState, Notebook, Output};
use std::time::{Duration, Instant};

struct Fixture {
    app: IdeApp,
    context: egui::Context,
    kernel: Option<CellRunner>,
    notebook: Notebook,
    root: tempfile::TempDir,
    python: PathBuf,
    process: Option<(u32, String)>,
}
impl Fixture {
    fn admitted() -> Self {
        assert_eq!(
            std::env::var("RUSTPY_SCIENTIFIC_ADMITTED").as_deref(),
            Ok("1")
        );
        assert!(!rustpy_settings::trust_all_from_env());
        let python =
            PathBuf::from(std::env::var_os("RUSTPY_SCIENTIFIC_PYTHON").expect("admitted SDK"));
        assert!(python.is_absolute() && python.is_file());
        let hash = rustpy_net::x509::sha256(&std::fs::read(&python).unwrap())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(
            hash,
            std::env::var("RUSTPY_SCIENTIFIC_PYTHON_SHA256").unwrap()
        );
        let temporary =
            dunce::canonicalize(PathBuf::from(std::env::var_os("TMPDIR").unwrap())).unwrap();
        for name in ["RUSTPY_CONFIG_DIR", "RUSTPY_SYSTEM_DIR", "RUSTPY_LOG_DIR"] {
            let path = PathBuf::from(std::env::var_os(name).expect(name));
            assert!(path.is_absolute() && path.starts_with(&temporary));
            std::fs::create_dir_all(&path).unwrap();
            assert!(dunce::canonicalize(&path).unwrap().starts_with(&temporary));
        }
        let root = tempfile::Builder::new()
            .prefix("scientific-λ-")
            .tempdir_in(temporary)
            .unwrap();
        let mut settings = Settings::default();
        settings.trust_project(root.path());
        settings.scientific.show_plots = true;
        settings.scientific.capture_interactive = true;
        let mut app = IdeApp::new(settings, Some(root.path().into()));
        let sdk = rustpy_project::interpreter::probe_detailed(&python, "configured-scientific")
            .unwrap()
            .interpreter;
        assert_eq!(sdk.path, python);
        assert!(
            sdk.version.starts_with("3.12."),
            "admitted Python3.12 required: {}",
            sdk.version
        );
        app.project.as_mut().unwrap().interpreter_rx = None;
        app.project.as_mut().unwrap().interpreter = Some(sdk);
        assert!(!app.project_safe_mode());
        Self {
            app,
            context: egui::Context::default(),
            kernel: None,
            notebook: Notebook::empty(),
            root,
            python,
            process: None,
        }
    }
    fn tick(&mut self) {
        if self.process.is_none() {
            self.process = std::fs::read(self.root.path().join("debuggee.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<(u32, String)>(&bytes).ok());
        }
        self.app.poll_run_workflows(&self.context);
        self.app.run_panel.poll();
        self.app.poll_debug(&self.context);
        if let Some(kernel) = &mut self.kernel {
            kernel.poll(&mut self.notebook);
        }
        self.app.poll_scientific(&self.context);
    }
    fn until(&mut self, description: &str, ready: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            self.tick();
            if ready(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{description}: status={:?}, debug={:?}, kernel={:?}",
                self.app.status,
                self.app.debug.as_ref().map(|session| &session.state),
                self.kernel.as_ref().map(CellRunner::state)
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn plots(&self) -> Vec<&PlotPayload> {
        (0..64)
            .map_while(|index| self.app.scientific.view.interactive(index))
            .collect()
    }
    fn debug_page(&mut self, expression: &str) {
        self.until("prior scientific request completes", |fixture| {
            !fixture.app.debug.as_ref().unwrap().backend.pending_view
        });
        let debug = self.app.debug.as_mut().unwrap();
        debug.backend.offset = 2;
        debug.backend.rows = 2;
        debug.backend.columns = 2;
        debug.backend.scientific_column_offset = 1;
        debug.inspect_value(expression, "table");
        self.until("genuine debugger page reaches SciView", |fixture| {
            fixture.app.scientific.view.data.iter().any(|tab| tab.name == expression && !tab.stale
                && matches!(tab.origin, Origin::Debug { epoch, .. } if epoch == fixture.app.debug.as_ref().unwrap().stop_epoch))
                && !fixture.app.debug.as_ref().unwrap().backend.pending_view
        });
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.app.cancel_run_workflows();
        if let Some(kernel) = &mut self.kernel {
            kernel.shutdown();
        }
        if let Some(debug) = &mut self.app.debug {
            debug.stop();
        }
        self.app.py_console.reset();
        for session in &mut self.app.run_panel.sessions {
            session.stop();
        }
        // Keep SDK/project/artifact/socket owners alive through failure unwind.
        loop {
            self.tick();
            if !self.app.run_workflows_pending()
                && !self.app.run_panel.has_running()
                && self
                    .kernel
                    .as_ref()
                    .is_none_or(|kernel| !kernel.owns_process())
                && self
                    .app
                    .debug
                    .as_ref()
                    .is_none_or(DebugSession::disconnect_completed)
                && rustpy_debug::dap::poll_retiring_transports().is_empty()
                && self
                    .process
                    .as_ref()
                    .is_none_or(|owner| !process_present(owner))
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn process_present((pid, start): &(u32, String)) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(") ")
                .map(|(_, tail)| tail.split_whitespace().nth(19).unwrap_or("").to_owned())
        })
        .is_some_and(|actual| &actual == start)
}

#[test]
#[ignore = "Root-admitted genuine Python NumPy/pandas/matplotlib/Plotly/Bokeh/debugpy and private profile"]
fn configured_debugger_scientific_producers_publish_real_pages_plots_and_retire_epochs() {
    let mut fixture = Fixture::admitted();
    let path = fixture.root.path().join("owned_scientific.py");
    let source = "import os, json, pathlib
pathlib.Path('debuggee.json').write_text(json.dumps([os.getpid(), pathlib.Path('/proc/self/stat').read_text().rsplit(') ',1)[1].split()[19]]))
import numpy as np\nimport pandas as pd\nimport matplotlib.pyplot as plt\nimport plotly.graph_objects as go\nfrom bokeh.plotting import figure, show\narray = np.arange(24).reshape(6, 4)\nframe = pd.DataFrame({'alpha': [10,11,12,13], 'beta': [20,21,22,23]})\nplt.plot([0,1],[1,42]); plt.title('SCIENTIFIC_MPL_42'); plt.show()\ngo.Figure(data=go.Scatter(y=[1,42]), layout={'title':'SCIENTIFIC_PLOTLY_42'}).show()\np = figure(title='SCIENTIFIC_BOKEH_42'); p.line([0,1],[1,42]); show(p)\nstage_one = 42\narray += 100\nframe['beta'] += 100\nstage_two = 43\nprint('SCIENTIFIC_FINISHED_42', flush=True)\n";
    std::fs::write(&path, source).unwrap();
    fixture.app.open_file(&path);
    for marker in ["stage_one =", "stage_two ="] {
        let line = source
            .lines()
            .position(|line| line.starts_with(marker))
            .unwrap()
            + 1;
        assert!(fixture.app.breakpoints.toggle(&path, line));
    }
    fixture.app.launch_config(
        RunConfig {
            name: "Scientific actual debug".into(),
            kind: RunConfigKind::Python {
                script: path.clone(),
            },
            interpreter: Some(fixture.python.clone()),
            working_dir: Some(fixture.root.path().into()),
            ..Default::default()
        },
        crate::run_context::RunMode::Debug,
    );
    let first = source
        .lines()
        .position(|line| line.starts_with("stage_one ="))
        .unwrap()
        + 1;
    fixture.until(
        "first real breakpoint and all genuine plot publishers",
        |fixture| {
            fixture.app.debug.as_ref().is_some_and(|debug| {
                debug.is_stopped()
                    && debug.current_frame().is_some_and(|frame| {
                        frame.path.as_ref() == Some(&path) && frame.line == first
                    })
            }) && fixture
                .plots()
                .iter()
                .any(|plot| plot.mime == "image/png" && plot.data.starts_with(b"\x89PNG\r\n\x1a\n"))
                && ["SCIENTIFIC_PLOTLY_42", "SCIENTIFIC_BOKEH_42"]
                    .iter()
                    .all(|marker| {
                        fixture.plots().iter().any(|plot| {
                            plot.mime == "text/html"
                                && String::from_utf8_lossy(&plot.data).contains(marker)
                        })
                    })
        },
    );
    let debug = fixture.app.debug.as_ref().unwrap();
    assert_eq!(debug.is_verified(&path, first), Some(true));
    let owner = debug.id;
    let epoch = debug.stop_epoch;
    fixture.debug_page("array");
    let tab = fixture
        .app
        .scientific
        .view
        .data
        .iter()
        .find(|tab| tab.name == "array")
        .unwrap();
    assert_eq!(
        tab.origin,
        Origin::Debug {
            session: owner,
            epoch
        }
    );
    assert_eq!(
        tab.page["rows"],
        serde_json::json!([["9", "10"], ["13", "14"]])
    );
    fixture.debug_page("frame");
    let tab = fixture
        .app
        .scientific
        .view
        .data
        .iter()
        .find(|tab| tab.name == "frame")
        .unwrap();
    assert_eq!(tab.page["columns"], serde_json::json!(["beta"]));
    assert_eq!(tab.page["rows"], serde_json::json!([["22"], ["23"]]));
    let pages = fixture.app.scientific.view.data.len();
    fixture.app.debug.as_mut().unwrap().continue_();
    let second = source
        .lines()
        .position(|line| line.starts_with("stage_two ="))
        .unwrap()
        + 1;
    fixture.until(
        "new suspension invalidates old scientific epoch",
        |fixture| {
            fixture.app.debug.as_ref().is_some_and(|debug| {
                debug.is_stopped()
                    && debug
                        .current_frame()
                        .is_some_and(|frame| frame.line == second)
            }) && fixture.app.scientific.view.data.iter().all(|tab| tab.stale)
        },
    );
    assert_ne!(fixture.app.debug.as_ref().unwrap().stop_epoch, epoch);
    fixture.debug_page("array");
    assert_eq!(
        fixture.app.scientific.view.data.len(),
        pages,
        "same owner updates existing data tab"
    );
    let tab = fixture
        .app
        .scientific
        .view
        .data
        .iter()
        .find(|tab| tab.name == "array")
        .unwrap();
    assert_eq!(
        tab.page["rows"],
        serde_json::json!([["109", "110"], ["113", "114"]])
    );
    fixture
        .app
        .debug
        .as_mut()
        .unwrap()
        .inspect_value("array", "table");
    fixture.app.debug.as_mut().unwrap().stop();
    fixture.until(
        "Stop physically retires DAP and invalidates data",
        |fixture| {
            fixture.app.debug.as_ref().unwrap().disconnect_completed()
                && fixture
                    .process
                    .as_ref()
                    .is_some_and(|owner| !process_present(owner))
                && fixture.app.scientific.view.data.iter().all(|tab| tab.stale)
        },
    );
    let count = fixture.app.scientific.view.data.len();
    fixture
        .app
        .scientific_event(Event::Refresh(0), &fixture.context);
    assert_eq!(fixture.app.scientific.view.data.len(), count);
    assert!(fixture.app.scientific.view.data[0]
        .message
        .as_deref()
        .unwrap()
        .contains("original idle console or suspended debugger"));
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);
}

#[test]
#[ignore = "Root-admitted genuine Python managed Jupyter/ipykernel NumPy/pandas/matplotlib and private profile"]
fn configured_managed_notebook_scientific_results_reach_guarded_app_consumers() {
    let mut fixture = Fixture::admitted();
    fixture.kernel = Some(
        CellRunner::start_with_connection(
            &fixture.python,
            fixture.root.path(),
            &[("MPLBACKEND".into(), "Agg".into())],
            "",
            Connection {
                kind: "managed".into(),
                kernel: std::env::var("RUSTPY_SCIENTIFIC_KERNEL")
                    .expect("explicit private SDK kernelspec"),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    fixture.until("genuine managed Jupyter ready", |fixture| {
        fixture.kernel.as_ref().unwrap().state() == KernelState::Idle
    });
    fixture.notebook.cells.push(Cell::code("import sys\nprint('SCIENTIFIC_KERNEL_SDK=' + sys.executable, flush=True)\nimport numpy as np\nimport pandas as pd\nimport matplotlib.pyplot as plt\nfrom IPython.display import display\narray = np.arange(24).reshape(6,4)\nframe = pd.DataFrame({'alpha':[10,11,12], 'beta':[20,21,22]})\nplt.plot([0,1],[1,42]); plt.title('ACTUAL_NOTEBOOK_SCIENTIFIC_42'); plt.show()\ndisplay(frame)\nprint('SCIENTIFIC_KERNEL_COMPLETED_42', flush=True)\n"));
    assert!(fixture
        .kernel
        .as_mut()
        .unwrap()
        .run_cell(&mut fixture.notebook, 0));
    fixture.until("real notebook scientific execution and PNG output", |fixture| {
        !fixture.kernel.as_ref().unwrap().is_busy() && fixture.notebook.cells[0].outputs.iter().any(|output|
            matches!(output, Output::DisplayData { data, .. } | Output::ExecuteResult { data, .. } if data.contains_key("image/png")))
    });
    assert!(
        fixture.notebook.cells[0]
            .outputs
            .iter()
            .all(|output| !matches!(output, Output::Error { .. })),
        "real scientific cell must not fail"
    );
    let transcript = fixture.notebook.cells[0]
        .outputs
        .iter()
        .filter_map(Output::plain_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        transcript.contains(&format!(
            "SCIENTIFIC_KERNEL_SDK={}",
            fixture.python.display()
        )),
        "managed kernel must use the admitted SDK: {transcript}"
    );
    assert!(transcript.contains("SCIENTIFIC_KERNEL_COMPLETED_42"));
    let path = fixture.root.path().join("actual_scientific.ipynb");
    fixture.notebook.save(&path).unwrap();
    fixture.app.open_file(&path);
    let index = fixture.app.active.unwrap();
    let request = fixture.kernel.as_mut().unwrap().request_data("frame", 1, 0);
    let mut page = None;
    let deadline = Instant::now() + Duration::from_secs(30);
    while page.is_none() {
        fixture.tick();
        for event in fixture.kernel.as_mut().unwrap().take_events() {
            if let rustpy_console::ConsoleEvent::Data {
                id, table, error, ..
            } = event
            {
                if id == request {
                    assert!(error.is_none(), "{error:?}");
                    page = table;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "genuine kernel data request did not complete"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let page = page.unwrap();
    assert_eq!(
        page.rows,
        [
            vec!["11".to_owned(), "21".to_owned()],
            vec!["12".to_owned(), "22".to_owned()]
        ]
    );
    // The envelope is the existing Notebook UI action; all data bytes above
    // came from the real managed kernel, not seeded model/protocol responses.
    fixture.app.notebook_event(
        index,
        &path,
        crate::notebook_view::NotebookEvent::Data(page),
    );
    let tab = fixture.app.scientific.view.data.last().unwrap();
    assert_eq!(tab.origin, Origin::Notebook(path.display().to_string()));
    assert_eq!(tab.page["row_start"], 1);
    let png = fixture.notebook.cells[0]
        .outputs
        .iter()
        .find_map(|output| match output {
            Output::DisplayData { data, .. } | Output::ExecuteResult { data, .. } => {
                data.get("image/png")
            }
            _ => None,
        })
        .unwrap();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(png)
        .unwrap();
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() > 1000);
    let plots = fixture.plots().len();
    for trusted in [false, true] {
        fixture.app.notebook_event(
            index,
            &path,
            crate::notebook_view::NotebookEvent::RichOutput {
                mime: "image/png".into(),
                data: bytes.clone(),
                trusted,
                title: "Actual managed scientific cell".into(),
            },
        );
        assert_eq!(fixture.plots().len(), plots + usize::from(trusted));
    }
    fixture.app.settings.trusted_projects.clear();
    fixture.app.settings.trusted_locations.clear();
    fixture.app.safe_mode = true;
    fixture.app.notebook_event(
        index,
        &path,
        crate::notebook_view::NotebookEvent::RichOutput {
            mime: "image/png".into(),
            data: bytes,
            trusted: true,
            title: "Must remain refused".into(),
        },
    );
    assert_eq!(
        fixture.plots().len(),
        plots + 1,
        "revoked project trust must block publication"
    );
    let snapshot = fixture
        .app
        .scientific
        .view
        .data
        .last()
        .unwrap()
        .page
        .clone();
    fixture.kernel.as_mut().unwrap().shutdown();
    fixture.until(
        "managed kernel/server/bridge physically retire",
        |fixture| !fixture.kernel.as_ref().unwrap().owns_process(),
    );
    fixture
        .app
        .scientific_event(Event::Refresh(0), &fixture.context);
    assert_eq!(
        fixture.app.scientific.view.data[0].page, snapshot,
        "retired notebook result remains a readable immutable snapshot"
    );
    assert!(fixture.app.scientific.view.data[0]
        .message
        .as_deref()
        .unwrap()
        .contains("Snapshots remain available"));
}
