//! Appearance & Behavior › System Settings › HTTP Proxy and Passwords, Tools ›
//! Server Certificates, the "Untrusted Server's Certificate" prompt and the
//! master password prompt. The logic lives in `crate::netsec`, `rustpy-net`
//! and `rustpy-secrets`; this module draws it and drives the password-safe
//! worker ([`crate::netsec::SafeWorker`]).

use super::*;
use crate::netsec::{self, SafeJob, SafeOutcome, SafeWorker};
use egui::RichText;
use rustpy_net::CertificateInfo;
use rustpy_secrets::{PasswordStorage, SafeError, SafeStatus};
use rustpy_settings::{ProxyKind, ProxyMode};

/// URL "Check connection" requests when none is entered.
const DEFAULT_CHECK_URL: &str = "https://pypi.org";

/// State of the network and password pages and prompts (part of `SettingsUi`).
#[derive(Default)]
pub(super) struct NetsecUi {
    started: bool,
    worker: Option<SafeWorker>,
    retired_outcomes: Vec<SafeOutcome>,
    retirement_error: Option<String>,
    /// The master-password prompt. Once dismissed it only comes back on request
    /// (Unlock… on the Passwords page, or work that needs a saved password).
    unlock: Option<UnlockPrompt>,
    unlock_dismissed: bool,
    /// The proxy password was requested from the safe for the settings page.
    proxy_password_requested: bool,
    /// Project whose deployment.json was checked for plain-text passwords.
    deployment_checked: Option<PathBuf>,
    // HTTP Proxy page.
    check_rx: Option<crossbeam_channel::Receiver<Result<String, String>>>,
    check_result: Option<Result<String, String>>,
    check_configuration: Option<rustpy_net::NetConfig>,
    // Server Certificates page.
    certs: Option<Vec<rustpy_net::AcceptedCertificate>>,
    certs_read_at: f64,
    selected_cert: Option<[u8; 32]>,
    import_path: String,
    cert_message: Option<Result<String, String>>,
    // Passwords page.
    status: Option<(SafeStatus, Result<Vec<String>, SafeError>)>,
    status_pending: bool,
    new_master: String,
    confirm_master: String,
    password_message: Option<Result<String, String>>,
}

struct UnlockPrompt {
    password: String,
    error: Option<String>,
    busy: bool,
}

impl UnlockPrompt {
    fn new() -> Self {
        UnlockPrompt {
            password: String::new(),
            error: None,
            busy: false,
        }
    }
}

/// `scheme://user:****@host:port/` — environment proxy URLs may carry passwords.
pub(super) fn redact_proxy_url(value: &str) -> String {
    let (scheme, rest) = match value.split_once("://") {
        Some((s, r)) => (format!("{s}://"), r),
        None => (String::new(), value),
    };
    let end = rest.find('/').unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    match authority.rsplit_once('@') {
        Some((userinfo, host)) => match userinfo.split_once(':') {
            Some((user, _)) => format!("{scheme}{user}:****@{host}{path}"),
            None => value.to_string(),
        },
        None => value.to_string(),
    }
}

/// The proxy variables of this process's environment (passwords hidden).
fn environment_proxies() -> Vec<String> {
    [
        "https_proxy",
        "HTTPS_PROXY",
        "http_proxy",
        "HTTP_PROXY",
        "all_proxy",
        "ALL_PROXY",
        "no_proxy",
        "NO_PROXY",
    ]
    .iter()
    .filter_map(|name| {
        rustpy_net::proxy::process_env(name)
            .map(|v| format!("{name}={}", redact_proxy_url(v.trim())))
    })
    .collect()
}

/// How an accepted certificate is trusted, for the Server Certificates list.
pub(super) fn trusted_for(cert: &rustpy_net::AcceptedCertificate) -> String {
    let mut parts = Vec::new();
    if cert.anchor {
        parts.push("certificate authority".to_string());
    }
    let session = rustpy_net::certs::session_hosts(&cert.info.sha256);
    let permanent: Vec<_> = cert
        .hosts
        .iter()
        .filter(|host| !session.contains(host))
        .cloned()
        .collect();
    if !permanent.is_empty() {
        parts.push(permanent.join(", "));
    }
    if !session.is_empty() {
        parts.push(format!("session only: {}", session.join(", ")));
    }
    parts.join("; ")
}

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir()
            .map(|h| h.join(rest))
            .unwrap_or_else(|| PathBuf::from(path)),
        None => PathBuf::from(path),
    }
}

fn message_label(ui: &mut egui::Ui, theme: &UiTheme, message: &Option<Result<String, String>>) {
    match message {
        Some(Ok(m)) => {
            ui.colored_label(theme.success, m);
        }
        Some(Err(e)) => {
            ui.colored_label(theme.error, e);
        }
        None => {}
    }
}

/// Subject, issuer, validity, names, key and fingerprints of a certificate.
fn certificate_details(
    ui: &mut egui::Ui,
    info: &CertificateInfo,
    theme: &UiTheme,
    id: impl egui::AsIdSalt,
) {
    if let Some(e) = &info.parse_error {
        ui.colored_label(
            theme.warning,
            format!("The certificate could not be read in full ({e})."),
        );
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([12.0, 3.0])
        .show(ui, |ui| {
            let row = |ui: &mut egui::Ui, name: &str, value: RichText| {
                ui.label(RichText::new(name).color(theme.text_dim));
                ui.add(egui::Label::new(value).wrap());
                ui.end_row();
            };
            if !info.subject.is_empty() {
                row(ui, "Issued to", RichText::new(info.subject.to_string()));
            }
            if !info.issuer.is_empty() {
                row(
                    ui,
                    "Issued by",
                    RichText::new(if info.self_signed {
                        format!("{} (self-signed)", info.issuer)
                    } else {
                        info.issuer.to_string()
                    }),
                );
            }
            if info.not_before.is_some() || info.not_after.is_some() {
                let validity = info.validity_text();
                let validity = if info.is_expired(now) {
                    RichText::new(format!("{validity} (expired)")).color(theme.error)
                } else if info.is_not_yet_valid(now) {
                    RichText::new(format!("{validity} (not valid yet)")).color(theme.error)
                } else {
                    RichText::new(validity)
                };
                row(ui, "Valid", validity);
            }
            let names = info.alt_names_text();
            if !names.is_empty() {
                row(ui, "Alternative names", RichText::new(names));
            }
            if info.is_ca {
                row(ui, "Type", RichText::new("Certificate authority"));
            }
            if !info.serial.is_empty() {
                row(ui, "Serial number", RichText::new(&info.serial).monospace());
            }
            if !info.public_key.is_empty() {
                row(ui, "Public key", RichText::new(&info.public_key));
            }
            if !info.signature_algorithm.is_empty() {
                row(ui, "Signature", RichText::new(&info.signature_algorithm));
            }
            row(
                ui,
                "SHA-256",
                RichText::new(info.sha256_hex()).monospace().small(),
            );
            row(
                ui,
                "SHA-1",
                RichText::new(info.sha1_hex()).monospace().small(),
            );
        });
}

impl IdeApp {
    /// Quit orchestration must drain serialized password writes before exit.
    pub(super) fn network_security_pending(&self) -> bool {
        self.settings_ui
            .netsec
            .worker
            .as_ref()
            .is_some_and(SafeWorker::is_busy)
    }

    /// Poll-only quit path. Keep completions private until normal UI resumes;
    /// never draw prompts, publish settings or enqueue follow-up operations here.
    /// Ok(true) means issued operations remain; Err requires aborting preparation.
    pub(super) fn network_security_retire(&mut self, ctx: &egui::Context) -> Result<bool, String> {
        let pending = self.network_security_retire_poll()?;
        if pending {
            ctx.request_repaint_after(std::time::Duration::from_millis(25));
        }
        Ok(pending)
    }

    /// Also used by the shell's readiness poll, which does not paint a frame.
    pub(super) fn network_security_retire_poll(&mut self) -> Result<bool, String> {
        let outcomes = self
            .settings_ui
            .netsec
            .worker
            .as_mut()
            .map(SafeWorker::poll)
            .unwrap_or_default();
        for outcome in outcomes {
            if self.settings_ui.netsec.retirement_error.is_none() {
                self.settings_ui.netsec.retirement_error = netsec::retirement_failure(&outcome);
            }
            self.settings_ui.netsec.retired_outcomes.push(outcome);
        }
        if let Some(error) = &self.settings_ui.netsec.retirement_error {
            return Err(error.clone());
        }
        Ok(self.network_security_pending())
    }

    /// Complete the one required settings write before a Quit guard is created.
    /// The vault already owns the migrated key. Keep all read results private and
    /// do not enqueue follow-up vault jobs or display unlock/certificate prompts.
    pub(super) fn finish_network_security_persistence(&mut self) -> Result<(), String> {
        if !self
            .settings_ui
            .netsec
            .retired_outcomes
            .iter()
            .any(|outcome| {
                matches!(
                    outcome,
                    SafeOutcome::Loaded {
                        legacy_moved: true,
                        ..
                    }
                )
            })
        {
            return Ok(());
        }
        self.settings.ai.legacy_api_key.clear();
        if let Some(snapshot) = &mut self.settings_ui.snapshot {
            snapshot.settings.ai.legacy_api_key.clear();
        }
        self.settings_to_persist().save().map_err(|error| {
            format!("Cannot remove the migrated plaintext credential from settings: {error}")
        })?;
        for outcome in &mut self.settings_ui.netsec.retired_outcomes {
            if let SafeOutcome::Loaded { legacy_moved, .. } = outcome {
                *legacy_moved = false;
            }
        }
        Ok(())
    }

    /// Each frame: starts the password-safe worker, applies its results, moves
    /// plain-text deployment passwords of a trusted project into the safe, and
    /// draws the certificate and master-password prompts.
    pub(super) fn netsec_tick(&mut self, ctx: &egui::Context) {
        if !self.settings_ui.netsec.started || self.settings_ui.netsec.worker.is_none() {
            self.settings_ui.netsec.started = true;
            if self.settings_ui.netsec.worker.is_none() {
                let repaint = ctx.clone();
                self.settings_ui.netsec.worker =
                    Some(SafeWorker::start(move || repaint.request_repaint()));
                self.safe_job(netsec::startup_load(&self.settings));
            }
        }
        if self.network_security_pending() {
            ctx.request_repaint_after(std::time::Duration::from_millis(25));
        }
        let mut outcomes = std::mem::take(&mut self.settings_ui.netsec.retired_outcomes);
        self.settings_ui.netsec.retirement_error = None;
        outcomes.extend(
            self.settings_ui
                .netsec
                .worker
                .as_mut()
                .map(SafeWorker::poll)
                .unwrap_or_default(),
        );
        for outcome in outcomes {
            self.on_safe_outcome(outcome);
        }
        if !self.safe_mode {
            if let Some(root) = self.project.as_ref().map(|p| p.root.clone()) {
                if self.settings_ui.netsec.deployment_checked.as_ref() != Some(&root) {
                    self.settings_ui.netsec.deployment_checked = Some(root.clone());
                    if rustpy_deploy::DeploymentConfig::path(&root).is_file() {
                        self.safe_job(SafeJob::MigrateDeployment(root));
                    }
                }
            }
        }
        if let Some(rx) = &self.settings_ui.netsec.check_rx {
            if let Ok(result) = rx.try_recv() {
                let current = netsec::net_config(&self.settings, &Settings::config_dir());
                self.settings_ui.netsec.check_result = Some(
                    if self.settings_ui.netsec.check_configuration.as_ref() == Some(&current) {
                        result
                    } else {
                        Err("Proxy/certificate settings changed while checking; check again".into())
                    },
                );
                self.settings_ui.netsec.check_rx = None;
            }
        }
        // A deployment transfer waits for the master password: ask, also after "Not now".
        if netsec::unlock_wanted() {
            self.ask_master_password();
        }
        let certificate = self.certificate_prompt(ctx);
        let unlock = self.unlock_prompt(ctx);
        if certificate || unlock {
            // The prompts are modal and run before everything else in the frame:
            // keys and text are theirs alone, so a master password can never
            // reach a shortcut, an editor or the AI chat behind them.
            ctx.input_mut(|i| {
                i.events.retain(|e| {
                    !matches!(
                        e,
                        egui::Event::Key { .. }
                            | egui::Event::Text(_)
                            | egui::Event::Paste(_)
                            | egui::Event::Copy
                            | egui::Event::Cut
                            | egui::Event::Ime(_)
                    )
                })
            });
        }
    }

    fn safe_job(&mut self, job: SafeJob) {
        self.settings_ui
            .netsec
            .worker
            .get_or_insert_with(|| SafeWorker::start(|| {}))
            .send(job);
    }

    /// Settings were applied (OK / Apply): moves the passwords when their
    /// storage changed, stores or forgets changed secrets, and reconfigures
    /// networking.
    pub(super) fn apply_netsec_settings(&mut self, old: &Settings) {
        let dir = Settings::config_dir();
        if old.passwords != self.settings.passwords {
            // Before the secrets below, so that they land in the new storage.
            self.safe_job(SafeJob::Switch {
                to: netsec::safe_config(&self.settings, &dir),
                previous: old.passwords.clone(),
            });
            self.settings_ui.netsec.status = None;
        }
        let ops = netsec::secret_ops(old, &self.settings);
        if !ops.is_empty() {
            self.safe_job(SafeJob::Apply(ops));
            self.settings_ui.netsec.status = None;
        }
        if (&old.http_proxy, &old.certificates)
            != (&self.settings.http_proxy, &self.settings.certificates)
        {
            rustpy_net::configure(netsec::net_config(&self.settings, &dir));
        }
    }

    /// Settings were re-read from disk (Import / Restore Defaults): secrets are
    /// not in settings.json, so the session's values come back from the safe.
    /// Settings that choose another password storage move the saved passwords
    /// there, as Apply does (installing an empty safe would orphan them).
    pub(super) fn netsec_settings_reloaded(&mut self) {
        if let Some(key) = rustpy_secrets::peek(rustpy_secrets::AI_API_KEY) {
            self.settings.ai.api_key = key;
        }
        if let Some(key) = rustpy_secrets::peek(rustpy_secrets::OPENAI_API_KEY) {
            self.settings.ai.openai_api_key = key;
        }
        if let Some(password) = rustpy_secrets::peek(rustpy_secrets::PROXY_PASSWORD) {
            self.settings.http_proxy.password = password;
        }
        let dir = Settings::config_dir();
        // Before the Load below, so that it reads from the new storage. If the
        // move fails, the storage in use is written back to the settings.
        if let Some(switch) = netsec::reload_storage_switch(&self.settings, &dir) {
            self.safe_job(switch);
        }
        netsec::configure_network(&mut self.settings, &dir);
        self.settings_ui.netsec.status = None;
        if self.settings_ui.netsec.started {
            self.safe_job(netsec::startup_load(&self.settings));
        }
    }

    /// Sets a session secret that is still empty, in the settings and in the
    /// Settings dialog's snapshot (so Cancel does not drop it).
    fn fill_session_secret(&mut self, field: fn(&mut Settings) -> &mut String, value: &str) {
        let current = field(&mut self.settings);
        if current.is_empty() {
            *current = value.to_string();
        }
        if let Some(snapshot) = &mut self.settings_ui.snapshot {
            let s = field(&mut snapshot.settings);
            if s.is_empty() {
                *s = value.to_string();
            }
        }
    }

    fn reconfigure_network(&self) {
        rustpy_net::configure(netsec::net_config(
            self.settings_to_persist(),
            &Settings::config_dir(),
        ));
    }

    fn ask_master_password(&mut self) {
        if self.settings_ui.netsec.unlock.is_none() {
            self.settings_ui.netsec.unlock = Some(UnlockPrompt::new());
        }
    }

    fn on_safe_outcome(&mut self, outcome: SafeOutcome) {
        match outcome {
            SafeOutcome::Loaded {
                legacy_moved,
                ai_key,
                proxy_password,
                openai_key,
            } => {
                if legacy_moved {
                    // The key now lives in the safe: drop the plain-text copy from settings.json.
                    self.settings.ai.legacy_api_key.clear();
                    if let Some(snapshot) = &mut self.settings_ui.snapshot {
                        snapshot.settings.ai.legacy_api_key.clear();
                    }
                    if let Err(e) = self.settings_to_persist().save() {
                        log::warn!("cannot save settings: {e}");
                    }
                }
                let mut locked = false;
                match ai_key {
                    Some(Ok(Some(key))) => self.fill_session_secret(|s| &mut s.ai.api_key, &key),
                    Some(Err(SafeError::Locked)) => locked = true,
                    Some(Err(e)) => self.set_status(format!(
                        "Cannot read the AI Assistant API key from the password safe: {e}"
                    )),
                    _ => {}
                }
                match openai_key {
                    Some(Ok(Some(key))) => {
                        self.fill_session_secret(|s| &mut s.ai.openai_api_key, &key)
                    }
                    Some(Err(SafeError::Locked)) => locked = true,
                    Some(Err(e)) => self.set_status(format!("Cannot read compatible API key: {e}")),
                    _ => {}
                }
                match proxy_password {
                    Some(Ok(Some(password))) => {
                        self.fill_session_secret(|s| &mut s.http_proxy.password, &password);
                        self.reconfigure_network();
                    }
                    Some(Err(SafeError::Locked)) => locked = true,
                    Some(Err(e)) => self.set_status(format!(
                        "Cannot read the proxy password from the password safe: {e}"
                    )),
                    _ => {}
                }
                if locked && !self.settings_ui.netsec.unlock_dismissed {
                    self.ask_master_password();
                }
            }
            SafeOutcome::Applied(results) => {
                let mut failed = Vec::new();
                for (op, result) in results {
                    match result {
                        Ok(()) => {}
                        Err(SafeError::Locked) => {
                            failed.push(format!(
                                "{}: the password database is locked",
                                netsec::secret_label(op.key())
                            ));
                            netsec::store_after_unlock(op);
                            self.ask_master_password();
                        }
                        Err(e) => failed.push(format!("{}: {e}", netsec::secret_label(op.key()))),
                    }
                }
                if !failed.is_empty() {
                    self.set_status(format!(
                        "Cannot update the password safe — {}",
                        failed.join("; ")
                    ));
                }
            }
            SafeOutcome::Switched { result, previous } => {
                self.settings_ui.netsec.status = None;
                match result {
                    Ok(n) => {
                        let message = format!("{n} saved password(s) moved to the new storage");
                        // Settings re-read from disk (an import) switch outside the dialog.
                        if n > 0 && !matches!(self.dialog, Dialog::Settings) {
                            self.set_status(message.clone());
                        }
                        self.settings_ui.netsec.password_message = Some(Ok(message));
                    }
                    Err(e) => {
                        // The old storage stays in use: show it in the settings again.
                        if matches!(e, SafeError::Locked) {
                            self.ask_master_password();
                        }
                        self.settings.passwords = previous.clone();
                        if let Some(snapshot) = &mut self.settings_ui.snapshot {
                            snapshot.settings.passwords = previous;
                        }
                        let _ = self.settings_to_persist().save();
                        let message = format!(
                            "The passwords were not moved, so the storage stays as it was: {e}"
                        );
                        if !matches!(self.dialog, Dialog::Settings) {
                            self.set_status(message.clone());
                        }
                        self.settings_ui.netsec.password_message = Some(Err(message));
                    }
                }
            }
            SafeOutcome::Unlocked(result) => {
                self.settings_ui.netsec.status = None;
                match result {
                    Ok(()) => {
                        self.settings_ui.netsec.unlock = None;
                        self.settings_ui.netsec.password_message =
                            Some(Ok("Password database unlocked".into()));
                        // Work waiting for the master password continues.
                        netsec::unlock_answered();
                        self.safe_job(netsec::startup_load(&self.settings));
                        let retry = netsec::take_stores_after_unlock();
                        if !retry.is_empty() {
                            self.safe_job(SafeJob::Apply(retry));
                        }
                        // Plain-text deployment passwords the locked database refused move now.
                        self.settings_ui.netsec.deployment_checked = None;
                    }
                    Err(e) => {
                        let text = if e == SafeError::WrongPassword {
                            "Wrong master password".to_string()
                        } else {
                            e.to_string()
                        };
                        match &mut self.settings_ui.netsec.unlock {
                            Some(prompt) => {
                                prompt.busy = false;
                                prompt.error = Some(text);
                            }
                            None => self.settings_ui.netsec.password_message = Some(Err(text)),
                        }
                    }
                }
            }
            SafeOutcome::MasterPasswordSet { set, result } => {
                self.settings_ui.netsec.status = None;
                self.settings_ui.netsec.password_message = Some(match result {
                    Ok(()) if set => {
                        Ok("Master password set: it is asked for once per session".into())
                    }
                    Ok(()) => Ok(
                        "Master password removed: the database is encrypted with the key file"
                            .into(),
                    ),
                    Err(e) => Err(format!("Cannot change the master password: {e}")),
                });
            }
            SafeOutcome::Status { status, keys } => {
                self.settings_ui.netsec.status_pending = false;
                self.settings_ui.netsec.status = Some((status, keys));
            }
            SafeOutcome::Deleted(key, result) => {
                self.settings_ui.netsec.status = None;
                self.settings_ui.netsec.password_message = Some(match result {
                    Ok(()) => Ok(format!("{} forgotten", netsec::secret_label(&key))),
                    Err(e) => Err(format!("Cannot remove {}: {e}", netsec::secret_label(&key))),
                });
                // A forgotten secret is no longer this session's either (nor
                // the Settings dialog's, which Cancel would put back).
                let field: Option<fn(&mut Settings) -> &mut String> = match key.as_str() {
                    rustpy_secrets::AI_API_KEY => Some(|s| &mut s.ai.api_key),
                    rustpy_secrets::OPENAI_API_KEY => Some(|s| &mut s.ai.openai_api_key),
                    rustpy_secrets::PROXY_PASSWORD => Some(|s| &mut s.http_proxy.password),
                    _ => None,
                };
                if let Some(field) = field {
                    field(&mut self.settings).clear();
                    if let Some(snapshot) = &mut self.settings_ui.snapshot {
                        field(&mut snapshot.settings).clear();
                    }
                    if key == rustpy_secrets::PROXY_PASSWORD {
                        self.reconfigure_network();
                    }
                }
            }
            SafeOutcome::WorkerFailed(error) => {
                self.settings_ui.netsec.worker = None;
                self.settings_ui.netsec.status_pending = false;
                self.settings_ui.netsec.password_message = Some(Err(error.clone()));
                self.set_status(error);
            }
            SafeOutcome::Message(m) => {
                if !m.is_empty() {
                    self.set_status(m);
                }
            }
        }
    }

    /// "Untrusted Server's Certificate" (modal): the oldest certificate waiting
    /// for a decision. Escape rejects it. Returns whether it is shown.
    fn certificate_prompt(&mut self, ctx: &egui::Context) -> bool {
        let prompts = rustpy_net::pending_prompts();
        let Some(prompt) = prompts.first() else {
            return false;
        };
        let theme = self.theme.clone();
        let mut answer: Option<rustpy_net::TrustDecision> = None;
        let modal = egui::Modal::new(egui::Id::new("untrusted_server_certificate")).show(ctx, |ui| {
            ui.set_width(620.0);
            ui.heading("Untrusted Server's Certificate");
            ui.label(RichText::new(format!("The server's certificate is not trusted: {}", prompt.host)).strong());
            ui.label(&prompt.problem);
            ui.add_space(4.0);
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for (i, cert) in prompt.chain.iter().enumerate() {
                    let title = if i == 0 { format!("Server certificate: {}", cert.display_name()) } else { format!("Sent with it: {}", cert.display_name()) };
                    egui::CollapsingHeader::new(title).id_salt(("cert_prompt_chain", prompt.id, i)).default_open(i == 0).show(ui, |ui| certificate_details(ui, cert, &theme, ("cert_prompt_grid", prompt.id, i)));
                }
            });
            ui.add_space(4.0);
            ui.label(RichText::new(format!("Accept it only if you know why the server uses this certificate, e.g. a self-signed development server. It is then trusted for {} only; to trust a company's certificate authority for every server, import its certificate in Settings › Tools › Server Certificates.", prompt.host)).color(theme.text_dim).small());
            if !prompt.waiting {
                ui.label(RichText::new("The request that met this certificate has failed; repeat it after accepting.").color(theme.warning).small());
            }
            if prompts.len() > 1 {
                ui.label(RichText::new(format!("{} more certificate(s) are waiting.", prompts.len() - 1)).color(theme.text_dim).small());
            }
            ui.horizontal(|ui| {
                if ui.button("Accept for this session").clicked() { answer=Some(rustpy_net::TrustDecision::Session); }
                if ui.button("Accept and save").clicked() { answer=Some(rustpy_net::TrustDecision::Permanent); }
                if ui.button("Reject").clicked() { answer=Some(rustpy_net::TrustDecision::Reject); }
            });
        });
        // A click outside does nothing (no accidental rejection); Escape rejects.
        if answer.is_none()
            && modal.is_top_modal
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            answer = Some(rustpy_net::TrustDecision::Reject);
        }
        if let Some(decision) = answer {
            let result = rustpy_net::answer_prompt_with(prompt.id, decision);
            self.settings_ui.netsec.certs = None;
            self.settings_ui.netsec.cert_message = Some(
                result
                    .clone()
                    .map(|_| "Certificate decision applied".into()),
            );
            match result {
                Err(error) => self.set_status(error),
                Ok(()) if decision != rustpy_net::TrustDecision::Reject => {
                    self.set_status(format!(
                        "Certificate of {} trusted{}",
                        prompt.host,
                        if decision == rustpy_net::TrustDecision::Session {
                            " for this session"
                        } else {
                            " and saved"
                        }
                    ))
                }
                _ => {}
            }
        }
        true
    }

    /// Asks for the password database's master password (modal). The field keeps
    /// the keyboard focus while the prompt is open: it is drawn early in the
    /// frame, so no widget behind the prompt (an editor, the AI chat input) ever
    /// receives what is typed. Escape or a click outside means "Not now".
    /// Returns whether it is shown.
    fn unlock_prompt(&mut self, ctx: &egui::Context) -> bool {
        let theme = self.theme.clone();
        let Some(st) = &mut self.settings_ui.netsec.unlock else {
            return false;
        };
        let (mut submit, mut dismiss) = (false, false);
        let modal = egui::Modal::new(egui::Id::new("password_safe_unlock")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.heading("Master Password");
            ui.label("The password database is protected by a master password. Enter it to use the saved passwords (AI Assistant key, proxy and deployment passwords).");
            if netsec::unlock_wanted() {
                ui.label(RichText::new("A deployment task is waiting for a saved password.").color(theme.text_dim));
            }
            let r = ui.add_enabled(!st.busy, egui::TextEdit::singleline(&mut st.password).password(true).hint_text("Master password").desired_width(f32::INFINITY));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                submit = true;
            } else if !st.busy && !r.has_focus() {
                r.request_focus();
            }
            if let Some(e) = &st.error {
                ui.colored_label(theme.error, e);
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(!st.busy && !st.password.is_empty(), egui::Button::new("Unlock")).clicked() {
                    submit = true;
                }
                if ui.button("Not now").clicked() {
                    dismiss = true;
                }
                if st.busy {
                    crate::a11y::spinner(ui);
                }
            });
        });
        if !submit && !st.busy && modal.should_close() {
            dismiss = true;
        }
        if submit && !st.busy && !st.password.is_empty() {
            st.busy = true;
            st.error = None;
            let password = std::mem::take(&mut st.password);
            self.safe_job(SafeJob::Unlock(password));
        } else if dismiss {
            self.settings_ui.netsec.unlock = None;
            self.settings_ui.netsec.unlock_dismissed = true;
            // Work waiting for the master password goes on without it.
            netsec::unlock_answered();
        }
        true
    }

    // ----- settings pages ------------------------------------------------------

    pub(super) fn page_http_proxy(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        ui.heading("HTTP Proxy");
        {
            let p = &mut self.settings.http_proxy;
            ui.radio_value(&mut p.mode, ProxyMode::NoProxy, "No proxy");
            ui.radio_value(
                &mut p.mode,
                ProxyMode::AutoDetect,
                "Auto-detect proxy settings",
            );
            ui.indent("proxy_auto", |ui| {
                let found = environment_proxies();
                let text = if found.is_empty() {
                    "From the environment (HTTPS_PROXY, HTTP_PROXY, ALL_PROXY, NO_PROXY): none set."
                        .to_string()
                } else {
                    format!("From the environment: {}", found.join("  "))
                };
                ui.label(RichText::new(text).color(theme.text_dim).small());
            });
            ui.radio_value(
                &mut p.mode,
                ProxyMode::Pac,
                "Automatic proxy configuration (PAC)",
            );
            ui.add_enabled_ui(p.mode == ProxyMode::Pac, |ui| {
                ui.horizontal(|ui| { ui.label("PAC URL or file:"); ui.add(egui::TextEdit::singleline(&mut p.pac_url).hint_text("https://proxy.example/proxy.pac or file:///path/proxy.pac").desired_width(420.0)); });
                ui.horizontal(|ui| { ui.label("Refresh after (seconds):"); ui.add(egui::DragValue::new(&mut p.pac_refresh_seconds).range(30..=86400)); if ui.button("Reload PAC").clicked() { rustpy_net::pac::clear_cache(); } });
                ui.horizontal(|ui| { ui.label("No proxy for:"); ui.text_edit_singleline(&mut p.no_proxy); });
                ui.checkbox(&mut p.authenticate,"PAC proxy authentication");
                ui.add_enabled_ui(p.authenticate, |ui| {
                    ui.horizontal(|ui| { ui.label("Login:"); ui.text_edit_singleline(&mut p.login); });
                    ui.horizontal(|ui| { ui.label("Password:"); ui.add(egui::TextEdit::singleline(&mut p.password).password(true)); ui.checkbox(&mut p.remember_password,"Remember in password safe"); });
                });
                ui.label("PAC is evaluated for each connection/request. Only explicit DIRECT directives bypass the proxy; credentials are sent only to the selected proxy. HTTPS passes host information, without its path/query.");
            });
            ui.radio_value(&mut p.mode, ProxyMode::Manual, "Manual proxy configuration");
            let manual = p.mode == ProxyMode::Manual;
            ui.indent("proxy_manual", |ui| {
                ui.add_enabled_ui(manual, |ui| {
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut p.kind, ProxyKind::Http, "HTTP");
                        ui.radio_value(&mut p.kind, ProxyKind::Socks, "SOCKS");
                    });
                    egui::Grid::new("http_proxy_grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                        ui.label("Host name:");
                        ui.add(egui::TextEdit::singleline(&mut p.host).hint_text("proxy.example.com").desired_width(280.0));
                        ui.end_row();
                        ui.label("Port number:");
                        ui.horizontal(|ui| {
                            ui.add(egui::DragValue::new(&mut p.port).range(0..=65535));
                            if p.port == 0 {
                                ui.label(RichText::new(format!("default ({})", p.kind.default_port())).color(theme.text_dim).small());
                            }
                        });
                        ui.end_row();
                        ui.label("No proxy for:");
                        ui.add(egui::TextEdit::singleline(&mut p.no_proxy).hint_text("*.domain.com, 192.168.*, 10.0.0.0/8").desired_width(380.0));
                        ui.end_row();
                    });
                    ui.label(RichText::new("Comma-separated host names, domains (.corp.example), * wildcards, IP addresses and CIDR ranges. localhost never uses the proxy.").color(theme.text_dim).small());
                    ui.checkbox(&mut p.authenticate, "Proxy authentication");
                    ui.add_enabled_ui(p.authenticate, |ui| {
                        egui::Grid::new("http_proxy_auth").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                            ui.label("Login:");
                            ui.add(egui::TextEdit::singleline(&mut p.login).desired_width(220.0));
                            ui.end_row();
                            ui.label("Password:");
                            ui.horizontal(|ui| {
                                ui.add(egui::TextEdit::singleline(&mut p.password).password(true).desired_width(220.0));
                                ui.checkbox(&mut p.remember_password, "Remember").on_hover_text("Keep the password in the password safe (Passwords page); otherwise it is forgotten when RustPy exits");
                            });
                            ui.end_row();
                        });
                    });
                });
            });
        }
        for problem in rustpy_net::proxy::validate(&self.settings.http_proxy) {
            ui.colored_label(theme.error, problem);
        }
        let p = &self.settings.http_proxy;
        if matches!(p.mode, ProxyMode::Manual | ProxyMode::Pac) && p.password_missing() {
            let busy = self
                .settings_ui
                .netsec
                .worker
                .as_ref()
                .is_some_and(SafeWorker::is_busy);
            let text = if netsec::needs_proxy_password(&self.settings) && busy {
                "Reading the proxy password from the password safe…"
            } else {
                "No proxy password: requests go to the proxy without credentials until one is entered (or the password database is unlocked)."
            };
            ui.colored_label(theme.warning, text);
        }
        ui.add_space(4.0);
        let route = if self.settings.http_proxy.mode == ProxyMode::Pac {
            "PAC (use Check to evaluate on a network worker)".into()
        } else {
            rustpy_net::proxy::choose(
                &self.settings.http_proxy,
                DEFAULT_CHECK_URL,
                &rustpy_net::proxy::process_env,
            )
            .describe()
        };
        ui.label(
            RichText::new(format!(
                "{DEFAULT_CHECK_URL} would be reached through: {route}"
            ))
            .color(theme.text_dim)
            .small(),
        );
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Check connection:");
            ui.add(egui::TextEdit::singleline(&mut self.settings.http_proxy.check_url).hint_text(DEFAULT_CHECK_URL).desired_width(300.0));
            let busy = self.settings_ui.netsec.check_rx.is_some();
            if ui.add_enabled(!busy, egui::Button::new("Check")).on_hover_text("Requests the URL with the settings on this page (also before they are applied)").clicked() {
                let url = Some(self.settings.http_proxy.check_url.trim().to_string()).filter(|u| !u.is_empty()).unwrap_or_else(|| DEFAULT_CHECK_URL.to_string());
                let cfg = netsec::net_config(&self.settings, &Settings::config_dir());
                self.settings_ui.netsec.check_configuration=Some(cfg.clone());
                let (tx, rx) = crossbeam_channel::bounded(1);
                let repaint = ui.ctx().clone();
                std::thread::spawn(move || {
                    let _ = tx.send(rustpy_net::check_connection(&cfg, &url));
                    repaint.request_repaint();
                });
                self.settings_ui.netsec.check_rx = Some(rx);
                self.settings_ui.netsec.check_result = None;
            }
            if busy {
                crate::a11y::spinner(ui);
            }
        });
        message_label(ui, &theme, &self.settings_ui.netsec.check_result);
        ui.add_space(4.0);
        ui.label(RichText::new("Used for PyPI and vulnerability lookups, the AI Assistant, the HTTP Client and issue trackers. A manual proxy is also passed to pip and uv as http(s)_proxy. Git and FTP deployment (curl) only follow the proxy variables of the environment RustPy was started in.").color(theme.text_dim).small());
        // Authentication switched on later: fetch the remembered password once.
        if netsec::needs_proxy_password(&self.settings)
            && !self.settings_ui.netsec.proxy_password_requested
        {
            self.settings_ui.netsec.proxy_password_requested = true;
            self.safe_job(SafeJob::Load {
                legacy_ai_key: None,
                ai_key: false,
                openai_key: false,
                proxy_password: true,
            });
        }
    }

    pub(super) fn page_server_certificates(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        let dir = netsec::certs_dir(&Settings::config_dir());
        let store = rustpy_net::CertStore::new(Some(dir.clone()));
        let now = ui.input(|i| i.time);
        {
            let st = &mut self.settings_ui.netsec;
            // Re-read now and then: prompts and other windows add certificates too.
            if st.certs.is_none() || now - st.certs_read_at > 3.0 {
                st.certs = Some(store.list());
                st.certs_read_at = now;
            }
        }
        ui.heading("Server Certificates");
        let c = &mut self.settings.certificates;
        ui.checkbox(
            &mut c.accept_automatically,
            "Accept non-trusted certificates automatically",
        );
        if c.accept_automatically {
            ui.colored_label(theme.warning, "Every certificate is then accepted without asking, so connections are no longer protected against impersonation (man-in-the-middle attacks).");
        }
        ui.checkbox(&mut c.use_system_store, "Trust the operating system's certificate authorities").on_hover_text(format!("{} (SSL_CERT_FILE and SSL_CERT_DIR too), on top of the bundled Mozilla root certificates", rustpy_net::certs::SYSTEM_STORE_DESCRIPTION));
        ui.label(RichText::new("The bundled Mozilla root certificates are always trusted. When a server's certificate cannot be verified, RustPy asks whether to accept it.").color(theme.text_dim).small());
        ui.add_space(6.0);
        ui.label(RichText::new("Accepted certificates").strong());
        let certs = self.settings_ui.netsec.certs.clone().unwrap_or_default();
        if certs.is_empty() {
            ui.label(RichText::new("None yet.").color(theme.text_dim));
        } else {
            egui::ScrollArea::vertical()
                .id_salt("accepted_certs")
                .max_height(180.0)
                .show(ui, |ui| {
                    egui::Grid::new("accepted_certs_grid")
                        .num_columns(5)
                        .striped(true)
                        .spacing([14.0, 3.0])
                        .show(ui, |ui| {
                            for h in ["Issued to", "Issued by", "Valid until", "Trusted for", ""] {
                                ui.label(RichText::new(h).strong());
                            }
                            ui.end_row();
                            for cert in &certs {
                                let selected =
                                    self.settings_ui.netsec.selected_cert == Some(cert.info.sha256);
                                if ui
                                    .selectable_label(selected, cert.info.display_name())
                                    .clicked()
                                {
                                    self.settings_ui.netsec.selected_cert = Some(cert.info.sha256);
                                }
                                ui.label(cert.info.issuer_name());
                                ui.label(
                                    cert.info
                                        .not_after
                                        .map(|t| rustpy_net::x509::format_time(t)[..10].to_string())
                                        .unwrap_or_default(),
                                );
                                ui.label(trusted_for(cert));
                                ui.label(RichText::new(&cert.note).color(theme.text_dim).small());
                                ui.end_row();
                            }
                        });
                });
        }
        let selected = self
            .settings_ui
            .netsec
            .selected_cert
            .and_then(|fp| certs.iter().find(|c| c.info.sha256 == fp));
        if let Some(cert) = selected {
            ui.add_space(4.0);
            egui::Frame::new()
                .fill(theme.hover_bg)
                .inner_margin(egui::Margin::same(6))
                .corner_radius(4)
                .show(ui, |ui| {
                    certificate_details(ui, &cert.info, &theme, "accepted_cert_details")
                });
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(selected.is_some(), egui::Button::new("Remove"))
                .clicked()
            {
                if let Some(cert) = selected {
                    self.settings_ui.netsec.cert_message =
                        Some(match store.remove(&cert.info.sha256) {
                            Ok(()) => Ok(format!("Removed {}", cert.info.display_name())),
                            Err(e) => Err(format!("Cannot remove the certificate: {e}")),
                        });
                    self.settings_ui.netsec.selected_cert = None;
                    self.settings_ui.netsec.certs = None;
                }
            }
            ui.separator();
            ui.add(
                egui::TextEdit::singleline(&mut self.settings_ui.netsec.import_path)
                    .hint_text("/path/to/certificate.pem (PEM or DER)")
                    .desired_width(300.0),
            );
            if ui
                .add_enabled(
                    !self.settings_ui.netsec.import_path.trim().is_empty(),
                    egui::Button::new("Import"),
                )
                .clicked()
            {
                let path = expand_home(self.settings_ui.netsec.import_path.trim());
                self.settings_ui.netsec.cert_message = Some(
                    store
                        .import_file(&path)
                        .map(|n| format!("Imported {n} certificate(s) from {}", path.display())),
                );
                self.settings_ui.netsec.certs = None;
                if matches!(self.settings_ui.netsec.cert_message, Some(Ok(_))) {
                    self.settings_ui.netsec.import_path.clear();
                }
            }
        });
        message_label(ui, &theme, &self.settings_ui.netsec.cert_message);
        ui.label(RichText::new(format!("A certificate accepted when RustPy asked is trusted for that server only. An imported certificate is trusted as a certificate authority: the certificates it issued are trusted for the names they are issued for. Stored as PEM files in {}; changes to this list take effect immediately.", dir.display())).color(theme.text_dim).small());
    }

    pub(super) fn page_passwords(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        let config_dir = Settings::config_dir();
        ui.heading("Passwords");
        {
            let pw = &mut self.settings.passwords;
            ui.radio_value(
                &mut pw.storage,
                PasswordStorage::Native,
                format!(
                    "In native keyring: {}",
                    rustpy_secrets::platform_keyring_name()
                ),
            );
            ui.indent("passwords_native", |ui| {
                ui.label(RichText::new("When the keyring is not available, passwords go to the encrypted file and move into the keyring once it works again.").color(theme.text_dim).small());
            });
            ui.radio_value(
                &mut pw.storage,
                PasswordStorage::EncryptedFile,
                "In an encrypted file (AES-256-GCM)",
            );
            ui.radio_value(
                &mut pw.storage,
                PasswordStorage::Memory,
                "Do not save, forget passwords after restart",
            );
            if pw.storage != PasswordStorage::Memory {
                ui.horizontal(|ui| {
                    ui.label("Database:");
                    ui.add(
                        egui::TextEdit::singleline(&mut pw.database)
                            .hint_text(config_dir.join("passwords.db").display().to_string())
                            .desired_width(380.0),
                    );
                });
            }
        }
        // The safe in use is the applied one; a changed choice moves the passwords on Apply.
        let applied = self
            .settings_ui
            .snapshot
            .as_ref()
            .map(|s| s.settings.passwords.clone())
            .unwrap_or_else(|| self.settings.passwords.clone());
        let changed = applied != self.settings.passwords;
        if changed {
            ui.colored_label(
                theme.accent,
                "Apply to move the saved passwords to the new storage.",
            );
        }
        ui.separator();
        let busy = self
            .settings_ui
            .netsec
            .worker
            .as_ref()
            .is_some_and(SafeWorker::is_busy);
        if self.settings_ui.netsec.status.is_none()
            && !self.settings_ui.netsec.status_pending
            && !busy
        {
            self.settings_ui.netsec.status_pending = true;
            self.safe_job(SafeJob::Status);
        }
        let status = self.settings_ui.netsec.status.clone();
        match &status {
            None => {
                ui.horizontal(|ui| {
                    crate::a11y::spinner(ui);
                    ui.label(RichText::new("Checking the password storage…").color(theme.text_dim));
                });
            }
            Some((status, keys)) => {
                ui.label(format!("Passwords are stored in: {}", status.location));
                if let Some(problem) = &status.problem {
                    ui.colored_label(theme.warning, format!("The keyring is not available ({problem}), so the encrypted file is used."));
                }
                let uses_file = applied.storage == PasswordStorage::EncryptedFile
                    || (applied.storage == PasswordStorage::Native && status.problem.is_some())
                    || status.master_password;
                if status.locked {
                    ui.horizontal(|ui| {
                        ui.colored_label(theme.warning, "The password database is locked.");
                        if ui.button("Unlock…").clicked() {
                            self.settings_ui.netsec.unlock_dismissed = false;
                            self.ask_master_password();
                        }
                    });
                }
                if uses_file && applied.storage != PasswordStorage::Memory {
                    ui.add_space(4.0);
                    ui.label(RichText::new("Master password").strong());
                    ui.label(RichText::new(if status.master_password { "The database is protected by a master password, asked for once per session.".to_string() } else { format!("No master password: the database is encrypted with a random key in {}.", config_dir.join("passwords.key").display()) }).color(theme.text_dim).small());
                    ui.add_enabled_ui(!changed && !status.locked && !busy, |ui| {
                        egui::Grid::new("master_password_grid")
                            .num_columns(2)
                            .spacing([10.0, 6.0])
                            .show(ui, |ui| {
                                ui.label(if status.master_password {
                                    "New master password:"
                                } else {
                                    "Master password:"
                                });
                                ui.add(
                                    egui::TextEdit::singleline(
                                        &mut self.settings_ui.netsec.new_master,
                                    )
                                    .password(true)
                                    .desired_width(240.0),
                                );
                                ui.end_row();
                                ui.label("Confirm:");
                                ui.add(
                                    egui::TextEdit::singleline(
                                        &mut self.settings_ui.netsec.confirm_master,
                                    )
                                    .password(true)
                                    .desired_width(240.0),
                                );
                                ui.end_row();
                            });
                        let (new, confirm) = (
                            &self.settings_ui.netsec.new_master,
                            &self.settings_ui.netsec.confirm_master,
                        );
                        let ready = !new.is_empty() && new == confirm;
                        if !confirm.is_empty() && new != confirm {
                            ui.colored_label(theme.error, "The passwords do not match");
                        }
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    ready,
                                    egui::Button::new(if status.master_password {
                                        "Change Master Password"
                                    } else {
                                        "Set Master Password"
                                    }),
                                )
                                .clicked()
                            {
                                let password =
                                    std::mem::take(&mut self.settings_ui.netsec.new_master);
                                self.settings_ui.netsec.confirm_master.clear();
                                self.safe_job(SafeJob::SetMasterPassword(Some(password)));
                            }
                            if status.master_password
                                && ui.button("Remove Master Password").clicked()
                            {
                                self.safe_job(SafeJob::SetMasterPassword(None));
                            }
                        });
                    });
                }
                ui.add_space(6.0);
                ui.label(RichText::new("Saved passwords").strong());
                match keys {
                    Ok(keys) if keys.is_empty() => {
                        ui.label(RichText::new("None.").color(theme.text_dim));
                    }
                    Ok(keys) => {
                        for key in keys {
                            ui.horizontal(|ui| {
                                ui.label(netsec::secret_label(key));
                                if ui
                                    .add_enabled(!busy, egui::Button::new("Forget").small())
                                    .clicked()
                                {
                                    self.safe_job(SafeJob::Delete(key.clone()));
                                }
                            });
                        }
                    }
                    Err(e) => {
                        ui.colored_label(
                            theme.warning,
                            format!("Cannot list the saved passwords: {e}"),
                        );
                    }
                }
            }
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!busy, egui::Button::new("Refresh"))
                .clicked()
            {
                self.settings_ui.netsec.status = None;
                self.settings_ui.netsec.password_message = None;
            }
            if busy {
                crate::a11y::spinner(ui);
            }
        });
        message_label(ui, &theme, &self.settings_ui.netsec.password_message);
        ui.label(RichText::new("Saved here: the AI Assistant API key, the HTTP proxy password and FTP deployment passwords. They are never written to settings.json or .rustpy/deployment.json.").color(theme.text_dim).small());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_urls_from_the_environment_hide_passwords() {
        assert_eq!(
            redact_proxy_url("http://me:s3cret@proxy.corp:3128/"),
            "http://me:****@proxy.corp:3128/"
        );
        assert_eq!(
            redact_proxy_url("socks5h://u:p@h:1080"),
            "socks5h://u:****@h:1080"
        );
        assert_eq!(
            redact_proxy_url("http://proxy.corp:3128"),
            "http://proxy.corp:3128"
        );
        assert_eq!(
            redact_proxy_url("me@proxy:8080"),
            "me@proxy:8080",
            "a user without a password stays"
        );
        assert_eq!(
            redact_proxy_url("user:pw@proxy:8080"),
            "user:****@proxy:8080"
        );
        assert_eq!(
            redact_proxy_url("http://a:b@c@host/x"),
            "http://a:****@host/x",
            "the last @ ends the credentials"
        );
    }

    #[test]
    fn trust_column() {
        let der = vec![0x30, 0x00];
        let cert = |hosts: &[&str], anchor| rustpy_net::AcceptedCertificate {
            path: None,
            der: der.clone(),
            info: CertificateInfo::from_der(&der),
            note: String::new(),
            hosts: hosts.iter().map(|h| h.to_string()).collect(),
            anchor,
        };
        assert_eq!(
            trusted_for(&cert(&["a.test", "b.test"], false)),
            "a.test, b.test"
        );
        assert_eq!(trusted_for(&cert(&[], true)), "certificate authority");
        assert_eq!(
            trusted_for(&cert(&["a.test"], true)),
            "certificate authority; a.test"
        );
    }

    #[test]
    fn home_is_expanded_in_import_paths() {
        if let Some(home) = dirs::home_dir() {
            assert_eq!(expand_home("~/ca.pem"), home.join("ca.pem"));
        }
        assert_eq!(expand_home("/etc/ca.pem"), PathBuf::from("/etc/ca.pem"));
    }
}
