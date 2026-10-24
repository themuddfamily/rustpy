//! Explicit local extractor execution. Snapshot is passed through a bounded file-free stdin.
use crate::workspace::Extractor;
use anyhow::{bail, Result};
use std::{
    io::Write,
    path::Path,
    process::{Child, Command, Stdio},
};
pub fn start(extractor: &Extractor, input: &str, output: &str, project: &Path) -> Result<Child> {
    if input.len() > 32 * 1024 * 1024 {
        bail!("Extractor input exceeds 32 MiB");
    }
    let file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output)?;
    let program = if extractor.program.is_absolute() {
        extractor.program.clone()
    } else {
        project.join(&extractor.program)
    };
    let mut child = Command::new(program)
        .args(&extractor.arguments)
        .current_dir(project)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(file))
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        let bytes = input.as_bytes().to_vec();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&bytes);
        });
    }
    Ok(child)
}
