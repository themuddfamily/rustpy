//! Background Git operations (roadmap §4.1): the running fetch/pull/push job
//! with its progress and Cancel, the Git console log of commands and their
//! output, and the ask-pass dialog that answers git's and ssh's prompts for
//! HTTPS credentials, key passphrases and unknown host keys.

use crate::a11y::DialogWindow;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use egui::Vec2;
use rustpy_vcs::askpass::{AskpassRequest, AskpassServer, PromptKind};
use rustpy_vcs::{GitEnv, GitJob, PushRequest};

use crate::theme::UiTheme;

pub fn copyable_command(args: &[String]) -> String {
    fn quote(value: &str) -> String {
        #[cfg(windows)]
        {
            format!("'{}'", value.replace('\'', "''"))
        }
        #[cfg(not(windows))]
        {
            format!("'{}'", value.replace('\'', "'\\''"))
        }
    }
    let command = std::iter::once(rustpy_vcs::git_program())
        .chain(args.iter().cloned())
        .map(|value| quote(&value))
        .collect::<Vec<_>>()
        .join(" ");
    #[cfg(windows)]
    {
        format!("& {command}")
    }
    #[cfg(not(windows))]
    {
        command
    }
}

/// What a background job is for (decides what happens when it ends).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitOp {
    Fetch,
    BranchNetwork {
        root: PathBuf,
        label: String,
        args: Vec<String>,
    },
    /// Pull / Update Project: the working tree changes.
    Pull,
    Push {
        request: PushRequest,
        commits: Option<usize>,
    },
}

impl GitOp {
    pub fn title(&self) -> String {
        match self {
            GitOp::Fetch => "Fetching".into(),
            GitOp::BranchNetwork { label, .. } => label.clone(),
            GitOp::Pull => "Updating project".into(),
            GitOp::Push { request, .. } => format!("Pushing to {}", request.target_label()),
        }
    }
}

pub struct RunningJob {
    pub root: PathBuf,
    pub id: u64,
    pub op: GitOp,
    pub job: GitJob,
}

/// A job that ended, for the app to react to.
#[derive(Clone, Debug)]
pub struct FinishedJob {
    pub root: PathBuf,
    pub op: GitOp,
    pub success: bool,
    pub stopped: bool,
    pub output: String,
    /// The most telling output line.
    pub summary: String,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ConsoleKind {
    Command,
    Output,
    Success,
    Failure,
}

#[derive(Clone, Debug)]
pub struct ConsoleLine {
    pub kind: ConsoleKind,
    pub text: String,
}

const CONSOLE_LIMIT: usize = 3000;

struct PendingPrompt {
    request: AskpassRequest,
    input: String,
    remember: bool,
    focus: bool,
}

/// Background Git jobs plus the ask-pass bridge (kept by the Git tool window).
#[derive(Default)]
pub struct GitJobs {
    hide_commands: bool,
    pub running: Option<RunningJob>,
    pub console: VecDeque<ConsoleLine>,
    server: Option<AskpassServer>,
    server_error: Option<String>,
    prompt: Option<PendingPrompt>,
    queue: VecDeque<AskpassRequest>,
    /// "Remember for this session": prompt text → answer.
    remembered: HashMap<String, String>,
    /// Remembered prompts each running job was answered with (forgotten again
    /// when the job fails to authenticate).
    answered: HashMap<u64, Vec<String>>,
    next_id: u64,
    background: Vec<(u64, std::sync::mpsc::Receiver<String>)>,
    finished: VecDeque<u64>,
}

impl GitJobs {
    /// Credential lifecycle for existing repository workers which report through
    /// the app's background status queues rather than owning a GitJob console tab.
    pub fn new_background_job(&mut self) -> (GitEnv, std::sync::mpsc::Sender<String>) {
        let (id, env) = self.new_job();
        let (tx, rx) = std::sync::mpsc::channel();
        self.background.push((id, rx));
        (env, tx)
    }
    pub fn set_show_commands(&mut self, show: bool) {
        self.hide_commands = !show;
    }

    /// Id and environment for a new git process: its prompts come to this IDE.
    pub fn new_job(&mut self) -> (u64, GitEnv) {
        self.next_id += 1;
        let id = self.next_id;
        if self.server.is_none() && self.server_error.is_none() {
            match AskpassServer::start() {
                Ok(s) => self.server = Some(s),
                Err(e) => {
                    log::warn!("askpass listener: {e}");
                    self.server_error = Some(e.to_string());
                }
            }
        }
        let helper: Option<PathBuf> = std::env::current_exe().ok();
        let vars = match (&self.server, helper) {
            (Some(s), Some(exe)) => s.env(&exe, id),
            _ => Vec::new(),
        };
        (id, GitEnv { vars })
    }

    pub fn is_busy(&self) -> bool {
        self.running.is_some()
    }

    /// Starts `git <args>` in `cwd` for `op`; one job at a time.
    pub fn start(&mut self, op: GitOp, cwd: &Path, args: Vec<String>) -> Result<(), String> {
        if let Some(r) = &self.running {
            return Err(format!("{} is still running", r.job.command_line));
        }
        let (id, env) = self.new_job();
        let job = GitJob::spawn(cwd, &args, &env).map_err(|e| format!("{e:#}"))?;
        self.log(ConsoleKind::Command, copyable_command(&args));
        self.running = Some(RunningJob {
            id,
            op,
            job,
            root: cwd.to_path_buf(),
        });
        Ok(())
    }

    /// Stops the running job.
    pub fn cancel(&mut self) {
        if let Some(r) = &mut self.running {
            r.job.stop();
        }
        // A prompt of the stopped job must not linger.
        if let Some(p) = self.prompt.take() {
            p.request.cancel();
        }
    }

    pub fn log(&mut self, kind: ConsoleKind, text: String) {
        if self.hide_commands && kind == ConsoleKind::Command {
            return;
        }
        self.console.push_back(ConsoleLine { kind, text });
        while self.console.len() > CONSOLE_LIMIT {
            self.console.pop_front();
        }
    }

    /// Reads the running job's output into the console; returns it once it ended.
    pub fn poll(&mut self) -> Option<FinishedJob> {
        let r = self.running.as_mut()?;
        let lines = r.job.poll();
        let done = !r.job.is_running();
        for l in lines {
            self.log(ConsoleKind::Output, l);
        }
        if !done {
            return None;
        }
        let r = self.running.take()?;
        let success = r.job.succeeded();
        let output = r.job.output();
        self.job_finished(r.id, success, &output);
        let summary = r.job.summary_line();
        let stopped = r.job.was_stopped();
        let note = if stopped {
            "Cancelled".to_string()
        } else if success {
            format!("Done in {:.1} s", r.job.elapsed().as_secs_f32())
        } else {
            format!("Failed (exit code {})", r.job.exit_code().unwrap_or(-1))
        };
        self.log(
            if success {
                ConsoleKind::Success
            } else {
                ConsoleKind::Failure
            },
            note,
        );
        Some(FinishedJob {
            root: r.root,
            op: r.op,
            success,
            stopped,
            output,
            summary,
        })
    }

    /// Bookkeeping when a git process started with [`Self::new_job`] ended:
    /// after an authentication failure the answers it was given from memory
    /// are forgotten, so the next attempt asks again.
    pub fn job_finished(&mut self, id: u64, success: bool, output: &str) {
        self.finished.push_back(id);
        while self.finished.len() > 512 {
            self.finished.pop_front();
        }
        if self
            .prompt
            .as_ref()
            .is_some_and(|prompt| prompt.request.job == id)
        {
            if let Some(prompt) = self.prompt.take() {
                prompt.request.cancel();
            }
        }
        let mut pending = VecDeque::new();
        while let Some(request) = self.queue.pop_front() {
            if request.job == id {
                request.cancel();
            } else {
                pending.push_back(request);
            }
        }
        self.queue = pending;
        let used = self.answered.remove(&id).unwrap_or_default();
        if !success && rustpy_vcs::is_auth_failure(output) {
            for prompt in used {
                self.remembered.remove(&prompt);
            }
        }
    }

    /// Takes new prompts from the ask-pass listener. Remembered answers are
    /// sent at once; notices are returned for the status bar.
    pub fn poll_prompts(&mut self) -> Vec<String> {
        let mut finished = Vec::new();
        self.background.retain(|(id, rx)| match rx.try_recv() {
            Ok(output) => {
                finished.push((*id, output));
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(_) => {
                finished.push((*id, String::new()));
                false
            }
        });
        for (id, output) in finished {
            self.job_finished(id, !rustpy_vcs::is_auth_failure(&output), &output);
        }
        let mut notices = Vec::new();
        while let Some(req) = self.server.as_ref().and_then(|s| s.try_recv()) {
            if self.finished.contains(&req.job) {
                req.cancel();
                continue;
            }
            match req.kind() {
                PromptKind::Info => notices.push(req.prompt.trim().to_string()),
                PromptKind::Secret | PromptKind::Text
                    if self.remembered.contains_key(&req.prompt) =>
                {
                    let answer = self.remembered[&req.prompt].clone();
                    self.answered
                        .entry(req.job)
                        .or_default()
                        .push(req.prompt.clone());
                    req.answer(&answer);
                }
                _ => self.queue.push_back(req),
            }
        }
        if self.prompt.is_none() {
            if let Some(request) = self.queue.pop_front() {
                self.prompt = Some(PendingPrompt {
                    request,
                    input: String::new(),
                    remember: false,
                    focus: true,
                });
            }
        }
        notices
    }

    pub fn has_prompt(&self) -> bool {
        self.prompt.is_some()
    }

    /// Answers the prompt on screen (`None` cancels it).
    fn respond(&mut self, answer: Option<String>) {
        let Some(p) = self.prompt.take() else { return };
        match answer {
            Some(a) => {
                if p.remember && matches!(p.request.kind(), PromptKind::Secret | PromptKind::Text) {
                    self.remembered.insert(p.request.prompt.clone(), a.clone());
                    self.answered
                        .entry(p.request.job)
                        .or_default()
                        .push(p.request.prompt.clone());
                }
                p.request.answer(&a);
            }
            None => p.request.cancel(),
        }
    }

    /// The ask-pass dialog for the prompt on screen (drawn every frame).
    pub fn prompt_window(&mut self, ctx: &egui::Context, theme: &UiTheme) {
        let Some(p) = &mut self.prompt else { return };
        let kind = p.request.kind();
        let title = match kind {
            PromptKind::YesNo => "Unknown SSH Host",
            PromptKind::Confirm => "Confirm",
            _ => "Git Authentication",
        };
        let mut decision: Option<Option<String>> = None;
        egui::Window::new(title)
            .id(egui::Id::new("git_askpass"))
            .collapsible(false)
            .resizable(false)
            .default_width(480.0)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .order(egui::Order::Foreground)
            .show_modal(ctx, |ui| {
                ui.set_max_width(560.0);
                ui.label(egui::RichText::new(p.request.prompt.trim()).color(theme.text));
                ui.add_space(4.0);
                match kind {
                    PromptKind::Secret | PromptKind::Text => {
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut p.input)
                                .password(kind == PromptKind::Secret)
                                .desired_width(f32::INFINITY),
                        );
                        if p.focus {
                            r.request_focus();
                            p.focus = false;
                        }
                        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            decision = Some(Some(p.input.clone()));
                        }
                        ui.checkbox(&mut p.remember, "Remember for this session");
                        ui.horizontal(|ui| {
                            if ui.button("OK").clicked() {
                                decision = Some(Some(p.input.clone()));
                            }
                            if ui.button("Cancel").clicked() {
                                decision = Some(None);
                            }
                        });
                    }
                    PromptKind::YesNo => {
                        ui.label(
                            egui::RichText::new("Connect and add the host key to known_hosts?")
                                .color(theme.text_dim)
                                .small(),
                        );
                        ui.horizontal(|ui| {
                            if ui.button("Yes").clicked() {
                                decision = Some(Some("yes".into()));
                            }
                            if ui.button("No").clicked() {
                                decision = Some(Some("no".into()));
                            }
                        });
                    }
                    PromptKind::Confirm | PromptKind::Info => {
                        ui.horizontal(|ui| {
                            if ui.button("OK").clicked() {
                                decision = Some(Some(String::new()));
                            }
                            if ui.button("Cancel").clicked() {
                                decision = Some(None);
                            }
                        });
                    }
                }
                ui.label(
                    egui::RichText::new("Asked by a Git operation started from the IDE.")
                        .color(theme.text_dim)
                        .small(),
                );
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    decision = Some(None);
                }
            });
        if let Some(d) = decision {
            self.respond(d);
        }
    }

    /// Spinner line for the Git tool window header: title, progress, Cancel.
    pub fn status_ui(&mut self, ui: &mut egui::Ui, theme: &UiTheme) {
        let Some(r) = &self.running else { return };
        let mut cancel = false;
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            crate::a11y::spinner(ui);
            ui.label(egui::RichText::new(format!("{}…", r.op.title())).color(theme.text));
            if let Some(p) = &r.job.progress {
                ui.label(egui::RichText::new(p).color(theme.text_dim).small());
            }
            if ui.small_button("Cancel").clicked() {
                cancel = true;
            }
        });
        if cancel {
            self.cancel();
        }
    }

    /// The Git console: commands the IDE ran and their output.
    pub fn console_ui(&mut self, ui: &mut egui::Ui, theme: &UiTheme) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Commands run by the IDE and their output")
                    .color(theme.text_dim)
                    .small(),
            );
            if ui.small_button("Clear").clicked() {
                self.console.clear();
            }
            if ui.small_button("Copy all").clicked() {
                ui.ctx().copy_text(
                    self.console
                        .iter()
                        .map(|line| line.text.as_str())
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("git_console")
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if self.console.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "No Git commands yet. Fetch, Pull, Push and Clone log here.",
                        )
                        .color(theme.text_dim),
                    );
                }
                for l in &self.console {
                    let color = match l.kind {
                        ConsoleKind::Command => theme.accent,
                        ConsoleKind::Output => theme.text,
                        ConsoleKind::Success => theme.success,
                        ConsoleKind::Failure => theme.error,
                    };
                    let response = ui.label(egui::RichText::new(&l.text).monospace().color(color));
                    response.context_menu(|ui| {
                        if ui.button("Copy").clicked() {
                            ui.ctx().copy_text(l.text.clone());
                            ui.close();
                        }
                    });
                }
            });
    }
}

/// A scratch directory for tests, removed on drop.
#[cfg(test)]
pub(crate) struct TestDir(PathBuf);

#[cfg(test)]
impl TestDir {
    pub(crate) fn new(name: &str) -> TestDir {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("rustpy-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TestDir(dir)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

#[cfg(test)]
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_vcs::askpass::{ask, ADDR_ENV, TOKEN_ENV};

    fn var(env: &GitEnv, k: &str) -> String {
        env.vars
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }

    fn pump_until_prompt(jobs: &mut GitJobs) {
        for _ in 0..500 {
            jobs.poll_prompts();
            if jobs.has_prompt() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("no prompt");
    }

    #[test]
    fn remembered_answers_are_reused_and_forgotten_after_an_auth_failure() {
        let mut jobs = GitJobs::default();
        let (id, env) = jobs.new_job();
        assert!(
            var(&env, "GIT_ASKPASS").len() > 1,
            "the IDE executable answers prompts"
        );
        let (addr, token) = (var(&env, ADDR_ENV), var(&env, TOKEN_ENV));
        let prompt = "Password for 'https://me@example.com': ";
        let ask_in_thread = |job: u64| {
            let (addr, token) = (addr.clone(), token.clone());
            std::thread::spawn(move || ask(&addr, &token, job, "", prompt).unwrap())
        };
        // First time: the user types it and ticks "Remember".
        let client = ask_in_thread(id);
        pump_until_prompt(&mut jobs);
        jobs.prompt.as_mut().unwrap().remember = true;
        jobs.respond(Some("s3cret".into()));
        assert_eq!(client.join().unwrap().as_deref(), Some("s3cret"));
        jobs.job_finished(id, true, "");
        // Second job: answered from memory, no dialog.
        let (id2, _) = jobs.new_job();
        let client = ask_in_thread(id2);
        let answer = loop {
            jobs.poll_prompts();
            assert!(!jobs.has_prompt(), "remembered prompts are not shown");
            if client.is_finished() {
                break client.join().unwrap();
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(answer.as_deref(), Some("s3cret"));
        // That job fails to authenticate: the answer is forgotten.
        jobs.job_finished(
            id2,
            false,
            "fatal: Authentication failed for 'https://example.com/'",
        );
        let (id3, _) = jobs.new_job();
        let client = ask_in_thread(id3);
        pump_until_prompt(&mut jobs);
        jobs.respond(None);
        assert_eq!(client.join().unwrap(), None, "cancel reaches git");
    }

    #[test]
    fn jobs_run_one_at_a_time_and_log_to_the_console() {
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let dir = crate::git_jobs::TestDir::new("jobs");
        let mut jobs = GitJobs::default();
        jobs.start(GitOp::Fetch, dir.path(), vec!["--version".into()])
            .unwrap();
        assert!(jobs
            .start(GitOp::Fetch, dir.path(), vec!["--version".into()])
            .is_err());
        let finished = loop {
            if let Some(f) = jobs.poll() {
                break f;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(finished.success);
        assert!(!jobs.is_busy());
        let texts: Vec<&str> = jobs.console.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts[0], copyable_command(&["--version".into()]));
        assert!(texts[1].starts_with("git version"));
        assert!(texts[2].starts_with("Done in"));
    }
}
