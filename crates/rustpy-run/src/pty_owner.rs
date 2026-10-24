//! Embedded terminals share run-session process groups and suspended ConPTY jobs.
use portable_pty::{Child, MasterPty, PtySize};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::Path,
};
pub struct PtyOwner(crate::process_tree::ProcessTree);
impl PtyOwner {
    fn attach(child: &mut (dyn Child + Send + Sync)) -> std::io::Result<Self> {
        crate::process_tree::ProcessTree::for_pty(child).map(Self)
    }
    pub fn poll(
        &self,
        child: &mut (dyn Child + Send + Sync),
    ) -> std::io::Result<Option<portable_pty::ExitStatus>> {
        self.0.poll_pty(child)
    }
    pub fn stop(&self, child: &mut (dyn Child + Send + Sync)) {
        self.0.force_pty(child);
    }
    /// Signal only; retain this owner and poll until physical retirement.
    pub fn request_stop(&self) {
        self.0.request_force();
    }
    pub fn stamp(&self) -> String {
        self.0.stamp()
    }
}
pub struct OwnedPty {
    pub owner: PtyOwner,
    pub child: Box<dyn Child + Send + Sync>,
    pub master: Box<dyn MasterPty + Send>,
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
}
impl OwnedPty {
    pub fn spawn(
        program: &str,
        args: &[String],
        cwd: &Path,
        env: &[(String, String)],
        size: PtySize,
    ) -> anyhow::Result<Self> {
        let mut variables: BTreeMap<String, String> = env.iter().cloned().collect();
        variables
            .entry("TERM".into())
            .or_insert_with(|| "xterm-256color".into());
        variables
            .entry("COLORTERM".into())
            .or_insert_with(|| "truecolor".into());
        variables
            .entry("TERM_PROGRAM".into())
            .or_insert_with(|| "RustPy".into());
        #[cfg(windows)]
        let parts = crate::windows_pty::spawn(Path::new(program), args, cwd, &variables, true)?;
        #[cfg(not(windows))]
        let parts = {
            let pair = portable_pty::native_pty_system().openpty(size)?;
            let reader = pair.master.try_clone_reader()?;
            let writer = pair.master.take_writer()?;
            let mut command = portable_pty::CommandBuilder::new(program);
            command.args(args);
            command.cwd(cwd);
            for (key, value) in &variables {
                command.env(key, value);
            }
            let child = pair.slave.spawn_command(command)?;
            drop(pair.slave);
            RawParts {
                child,
                master: pair.master,
                reader,
                writer,
            }
        };
        let mut child = parts.child;
        let owner = match PtyOwner::attach(child.as_mut()) {
            Ok(owner) => owner,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        };
        let _ = parts.master.resize(size);
        Ok(Self {
            owner,
            child,
            master: parts.master,
            reader: parts.reader,
            writer: parts.writer,
        })
    }
}
#[cfg(not(windows))]
struct RawParts {
    child: Box<dyn Child + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
}
