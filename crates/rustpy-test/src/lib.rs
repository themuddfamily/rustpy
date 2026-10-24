//! RustPy IDE — test discovery and test running (roadmap §3.4).
//!
//! * [`discover_file`] / [`discover_project`]: static discovery of pytest-style
//!   and `unittest` tests from the syntax tree (no Python needed);
//!   [`discover_features`] / [`discover_feature_file_in`] for behave
//!   `.feature` files.
//! * [`detect_framework`]: pytest vs. unittest for a project/interpreter;
//!   [`detect_framework_for`] also picks behave / Twisted Trial for targets.
//! * [`TestRun`]: runs tests and turns their output into [`TestEvent`]s
//!   polled with [`TestRun::poll`]. pytest and unittest run through an
//!   embedded Python reporter that streams JSON events
//!   (`##rustpy-test## {...}` lines); [`Framework::Behave`] runs
//!   `python -m behave` with a `json.pretty` report file (plus the `plain`
//!   formatter on the console for live progress), [`Framework::Trial`] runs
//!   `python -m twisted.trial --reporter=timing` and parses its text output.
//! * [`parse_behave_json`] / [`parse_trial_output`]: the same conversions for
//!   recorded output.
//! * [`parse_failure_location`]: finds the project frame of a failure for
//!   "jump to source".
//!
//! Test ids use the pytest node-id form everywhere, relative to the project
//! root: `tests/test_x.py::TestA::test_b` (nested classes add more `::`
//! segments). The same ids are produced for `unittest` and Trial runs, so the
//! UI can match results to discovered items regardless of framework. behave
//! ids are `features/x.feature::Feature name::Scenario name` (the feature is
//! the suite, every scenario / outline row a test).

mod behave;
mod discover;
pub mod integration;
pub use integration::{is_test_file_in, tox_environment_plans, PytestConfiguration};
pub mod related;
mod runner;
mod trial;

pub use behave::{
    behave_args, behave_feature_id, behave_targets, discover_feature_file_in, discover_features,
    is_feature_file, parse_behave_json, parse_behave_report, project_uses_behave, BehaveIndex,
};
pub use discover::{
    discover_file, discover_file_in, discover_parsed_in, discover_project, is_test_file,
    node_id_to_unittest_name, TestItem, TestKind,
};
pub use runner::{
    behave_available, configured_framework, detect_framework, detect_framework_for,
    parse_failure_location, project_uses_pytest, project_uses_trial, provision_reporter,
    pytest_available, trial_available, Framework, Outcome, Summary, TestEvent, TestMetadata,
    TestResult, TestRun, TestRunOptions,
};
pub use trial::{
    file_uses_trial, parse_trial_output, trial_args, trial_module_name, TrialIds, TrialParser,
    TRIAL_REPORTER,
};

mod debug_events;
