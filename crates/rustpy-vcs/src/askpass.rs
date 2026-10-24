//! Ask-pass bridge (roadmap §4.1 "Git credential helpers & SSH agent
//! integration (ask-pass dialog …)").
//!
//! Git and ssh ask for HTTPS credentials, key passphrases and host-key
//! confirmations by running the program named in `GIT_ASKPASS` /
//! `SSH_ASKPASS` with the prompt as its argument and reading the answer from
//! its stdout. The IDE names its own executable there: started that way, the
//! binary does not open a window but forwards the prompt to the running IDE
//! over a loopback socket ([`helper_main`]) and prints the reply. The IDE
//! side ([`AskpassServer`]) hands each prompt to the UI as an
//! [`AskpassRequest`]. Requests must carry a per-session random token, so
//! other local processes cannot make the IDE show a prompt.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

/// Address of the IDE's ask-pass listener (`127.0.0.1:port`).
pub const ADDR_ENV: &str = "RUSTPY_ASKPASS_ADDR";
/// Token the helper must send with each request.
pub const TOKEN_ENV: &str = "RUSTPY_ASKPASS_TOKEN";
/// Id of the IDE job the git process belongs to.
pub const JOB_ENV: &str = "RUSTPY_ASKPASS_JOB";
const MAGIC: &str = "RUSTPY-ASKPASS/1";

/// How a prompt is answered.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PromptKind {
    /// A password, passphrase, PIN or token: the typed text is hidden.
    Secret,
    /// A user name or other visible text.
    Text,
    /// A yes/no question (ssh's unknown host key): answered `yes` or `no`.
    YesNo,
    /// `SSH_ASKPASS_PROMPT=confirm`: OK allows, Cancel refuses.
    Confirm,
    /// `SSH_ASKPASS_PROMPT=none`: a notice only ("touch your security key").
    Info,
}

/// Classifies a prompt from its text and ssh's `SSH_ASKPASS_PROMPT` hint.
pub fn prompt_kind(prompt: &str, hint: &str) -> PromptKind {
    match hint {
        "confirm" => return PromptKind::Confirm,
        "none" => return PromptKind::Info,
        _ => {}
    }
    let lower = prompt.to_lowercase();
    if lower.contains("(yes/no") {
        PromptKind::YesNo
    } else if lower.trim_start().starts_with("username") {
        PromptKind::Text
    } else {
        PromptKind::Secret
    }
}

/// One prompt from a git or ssh process, waiting for the user's answer.
/// Dropping it without answering cancels the prompt.
pub struct AskpassRequest {
    pub prompt: String,
    /// ssh's `SSH_ASKPASS_PROMPT` (`confirm`, `none` or empty).
    pub hint: String,
    /// The job id the git process was started with (0 when unknown).
    pub job: u64,
    reply: Sender<Option<String>>,
}

impl AskpassRequest {
    pub fn kind(&self) -> PromptKind {
        prompt_kind(&self.prompt, &self.hint)
    }

    /// Sends `text` to the waiting process.
    pub fn answer(self, text: &str) {
        let _ = self.reply.send(Some(text.to_string()));
    }

    /// Refuses the prompt: git/ssh treat it as a failed authentication.
    pub fn cancel(self) {
        let _ = self.reply.send(None);
    }
}

impl std::fmt::Debug for AskpassRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AskpassRequest")
            .field("prompt", &self.prompt)
            .field("hint", &self.hint)
            .field("job", &self.job)
            .finish()
    }
}

/// The IDE side of the bridge: a loopback listener plus the queue of prompts.
pub struct AskpassServer {
    addr: SocketAddr,
    token: String,
    rx: Receiver<AskpassRequest>,
    closed: Arc<AtomicBool>,
}

impl AskpassServer {
    pub fn start() -> std::io::Result<AskpassServer> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let addr = listener.local_addr()?;
        let token = random_token();
        let closed = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let (expected, stop) = (token.clone(), closed.clone());
        std::thread::Builder::new()
            .name("askpass".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    let (tx, expected) = (tx.clone(), expected.clone());
                    std::thread::spawn(move || serve(stream, &expected, &tx));
                }
            })?;
        Ok(AskpassServer {
            addr,
            token,
            rx,
            closed,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Environment that routes the prompts of a git process started for
    /// `job` to this server, through `helper` (the IDE executable).
    pub fn env(&self, helper: &Path, job: u64) -> Vec<(String, String)> {
        let helper = helper.display().to_string();
        vec![
            ("GIT_ASKPASS".into(), helper.clone()),
            ("SSH_ASKPASS".into(), helper),
            // OpenSSH 8.4+: use the helper even when a terminal is around.
            ("SSH_ASKPASS_REQUIRE".into(), "force".into()),
            (ADDR_ENV.into(), self.addr.to_string()),
            (TOKEN_ENV.into(), self.token.clone()),
            (JOB_ENV.into(), job.to_string()),
        ]
    }

    /// The next prompt waiting for the user, if any.
    pub fn try_recv(&self) -> Option<AskpassRequest> {
        self.rx.try_recv().ok()
    }
}

impl Drop for AskpassServer {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Relaxed);
        // Wake the accept loop so its thread ends.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
    }
}

fn serve(stream: TcpStream, expected: &str, tx: &Sender<AskpassRequest>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let mut field = || -> Option<String> {
        let mut line = String::new();
        reader.read_line(&mut line).ok().filter(|n| *n > 0)?;
        Some(unescape(line.trim_end_matches(['\n', '\r'])))
    };
    let (Some(magic), Some(token), Some(job), Some(hint), Some(prompt)) =
        (field(), field(), field(), field(), field())
    else {
        return;
    };
    if magic != MAGIC || token.is_empty() || token != expected {
        log::warn!("askpass: rejected a request with a wrong token");
        return;
    }
    let job = job.parse().unwrap_or(0);
    let (reply_tx, reply_rx) = channel();
    let notice = hint == "none";
    if tx
        .send(AskpassRequest {
            prompt,
            hint,
            job,
            reply: reply_tx,
        })
        .is_err()
    {
        return;
    }
    // A notice needs no answer: ssh kills the helper when it is done.
    let answer = if notice {
        Some(String::new())
    } else {
        reply_rx.recv().ok().flatten()
    };
    let mut w = stream;
    let _ = match answer {
        Some(a) => write!(w, "OK\n{}\n", escape(&a)),
        None => w.write_all(b"CANCEL\n"),
    };
    let _ = w.flush();
}

/// Client side: sends one prompt to the IDE and waits for the answer
/// (`None`: the user cancelled).
pub fn ask(
    addr: &str,
    token: &str,
    job: u64,
    hint: &str,
    prompt: &str,
) -> std::io::Result<Option<String>> {
    let mut stream = TcpStream::connect(addr)?;
    write!(
        stream,
        "{MAGIC}\n{}\n{job}\n{}\n{}\n",
        escape(token),
        escape(hint),
        escape(prompt)
    )?;
    stream.flush()?;
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status)?;
    if status.trim_end() != "OK" {
        return Ok(None);
    }
    let mut answer = String::new();
    reader.read_line(&mut answer)?;
    Ok(Some(unescape(answer.trim_end_matches(['\n', '\r']))))
}

/// In-process libgit2 credential callbacks use the same authenticated UI bridge,
/// with bounded reads so cancelling a transfer also cancels a waiting prompt.
pub fn ask_cancellable(
    env: &crate::GitEnv,
    cancel: &AtomicBool,
    hint: &str,
    prompt: &str,
) -> std::io::Result<Option<String>> {
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }
    let value = |key: &str| {
        env.vars
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    let addr = value(ADDR_ENV)
        .and_then(|s| s.parse::<SocketAddr>().ok())
        .filter(|a| a.ip().is_loopback())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "The IDE credential bridge is unavailable",
            )
        })?;
    let token = value(TOKEN_ENV).filter(|s| !s.is_empty()).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Missing IDE credential bridge token",
        )
    })?;
    let job = value(JOB_ENV)
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.set_read_timeout(Some(Duration::from_millis(200)))?;
    write!(
        stream,
        "{MAGIC}\n{}\n{job}\n{}\n{}\n",
        escape(token),
        escape(hint),
        escape(prompt)
    )?;
    stream.flush()?;
    let mut reader = BufReader::new(stream);
    let deadline = std::time::Instant::now() + Duration::from_secs(300);
    let mut line = String::new();
    let mut status = None;
    loop {
        if cancel.load(Ordering::Relaxed) || std::time::Instant::now() >= deadline {
            return Ok(None);
        }
        match reader.read_line(&mut line) {
            Ok(0) => return Ok(None),
            Ok(_) => {
                if line.len() > 64 * 1024 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Credential reply is too long",
                    ));
                }
                if !line.ends_with('\n') {
                    continue;
                }
                if status.is_none() {
                    if line.trim_end() != "OK" {
                        return Ok(None);
                    }
                    status = Some(());
                    line.clear();
                } else {
                    return Ok(Some(unescape(line.trim_end_matches(['\n', '\r']))));
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(e),
        }
    }
}

/// Runs the helper when this process was started as `GIT_ASKPASS` /
/// `SSH_ASKPASS` by a git process of the IDE: returns the exit code to use,
/// or `None` for a normal start. Call it first thing in `main`.
pub fn helper_main() -> Option<i32> {
    let addr = std::env::var(ADDR_ENV)
        .ok()
        .filter(|a| !a.trim().is_empty())?;
    let token = std::env::var(TOKEN_ENV).unwrap_or_default();
    let job = std::env::var(JOB_ENV)
        .ok()
        .and_then(|j| j.parse().ok())
        .unwrap_or(0);
    let hint = std::env::var("SSH_ASKPASS_PROMPT").unwrap_or_default();
    let prompt = std::env::args().nth(1).unwrap_or_default();
    Some(match ask(&addr, &token, job, &hint, &prompt) {
        Ok(Some(answer)) => {
            let mut out = std::io::stdout();
            let _ = writeln!(out, "{answer}");
            let _ = out.flush();
            0
        }
        _ => 1,
    })
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// 128 unpredictable bits as hex: the OS random source on Unix, else
/// randomly keyed SipHash over the time and process id.
fn random_token() -> String {
    #[cfg(unix)]
    {
        use std::io::Read;
        let mut bytes = [0u8; 16];
        if std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut bytes))
            .is_ok()
        {
            return bytes.iter().map(|b| format!("{b:02x}")).collect();
        }
    }
    use std::hash::{BuildHasher, Hasher};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    (0..2u64)
        .map(|i| {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u64(i);
            h.write_u128(nanos);
            h.write_u32(std::process::id());
            format!("{:016x}", h.finish())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelling_before_a_builtin_prompt_never_contacts_the_bridge() {
        assert!(ask_cancellable(
            &crate::GitEnv::default(),
            &AtomicBool::new(true),
            "",
            "Password:"
        )
        .unwrap()
        .is_none());
    }

    fn wait_request(server: &AskpassServer) -> AskpassRequest {
        for _ in 0..500 {
            if let Some(r) = server.try_recv() {
                return r;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("no request arrived");
    }

    #[test]
    fn prompts_round_trip_through_the_server() {
        let server = AskpassServer::start().unwrap();
        let env = server.env(Path::new("/opt/rustpy"), 7);
        let get = |k: &str| {
            env.iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert_eq!(get("GIT_ASKPASS"), "/opt/rustpy");
        assert_eq!(get("SSH_ASKPASS_REQUIRE"), "force");
        let (addr, token) = (get(ADDR_ENV), get(TOKEN_ENV));
        assert_eq!(token.len(), 32);
        let prompt = "The authenticity of host 'github.com' can't be established.\nAre you sure you want to continue connecting (yes/no/[fingerprint])? ";
        let client = {
            let (addr, token) = (addr.clone(), token.clone());
            std::thread::spawn(move || ask(&addr, &token, 7, "", prompt).unwrap())
        };
        let req = wait_request(&server);
        assert_eq!(req.prompt, prompt, "newlines survive the transport");
        assert_eq!(req.job, 7);
        assert_eq!(req.kind(), PromptKind::YesNo);
        req.answer("yes");
        assert_eq!(client.join().unwrap().as_deref(), Some("yes"));
        // Cancel.
        let client = {
            let (addr, token) = (addr.clone(), token.clone());
            std::thread::spawn(move || {
                ask(
                    &addr,
                    &token,
                    7,
                    "",
                    "Password for 'https://me@example.com': ",
                )
                .unwrap()
            })
        };
        let req = wait_request(&server);
        assert_eq!(req.kind(), PromptKind::Secret);
        req.cancel();
        assert_eq!(client.join().unwrap(), None);
        // A secret with a backslash and a dropped request (cancel by drop).
        let client = {
            let (addr, token) = (addr.clone(), token.clone());
            std::thread::spawn(move || {
                ask(&addr, &token, 1, "", "Enter passphrase for key '/k': ").unwrap()
            })
        };
        wait_request(&server).answer("p\\a ss\\n");
        assert_eq!(client.join().unwrap().as_deref(), Some("p\\a ss\\n"));
        let client = std::thread::spawn(move || {
            ask(&addr, &token, 1, "", "Username for 'https://example.com': ").unwrap()
        });
        drop(wait_request(&server));
        assert_eq!(client.join().unwrap(), None);
    }

    #[test]
    fn a_wrong_token_is_refused_without_prompting() {
        let server = AskpassServer::start().unwrap();
        let addr = server.addr().to_string();
        let answer = ask(&addr, "not-the-token", 1, "", "Password: ").unwrap();
        assert_eq!(answer, None);
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            server.try_recv().is_none(),
            "the UI never saw the forged prompt"
        );
    }

    #[test]
    fn notices_do_not_wait_for_the_user() {
        let server = AskpassServer::start().unwrap();
        let env = server.env(Path::new("x"), 3);
        let get = |k: &str| {
            env.iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        let answer = ask(
            &get(ADDR_ENV),
            &get(TOKEN_ENV),
            3,
            "none",
            "Confirm user presence for key ED25519-SK",
        )
        .unwrap();
        assert_eq!(answer.as_deref(), Some(""));
        let req = wait_request(&server);
        assert_eq!(req.kind(), PromptKind::Info);
    }

    #[test]
    fn prompt_kinds() {
        assert_eq!(
            prompt_kind("Username for 'https://github.com': ", ""),
            PromptKind::Text
        );
        assert_eq!(
            prompt_kind("Password for 'https://u@github.com': ", ""),
            PromptKind::Secret
        );
        assert_eq!(
            prompt_kind("Enter passphrase for key '/home/u/.ssh/id_ed25519': ", ""),
            PromptKind::Secret
        );
        assert_eq!(
            prompt_kind("Allow use of key?", "confirm"),
            PromptKind::Confirm
        );
        assert_eq!(escape("a\\b\nc"), "a\\\\b\\nc");
        assert_eq!(unescape(&escape("a\\b\nc\r")), "a\\b\nc\r");
    }
}
