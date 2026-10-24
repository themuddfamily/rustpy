//! ROOT-admitted genuine Pandoc DOCX export through the production Markdown
//! action, captured RunPanel process and guarded publication. No fake executable,
//! GUI, network resource or extra PDF engine. Run this one ignored selector alone.
use super::*;
use std::{
    io::Read,
    time::{Duration, Instant},
};

struct Fixture {
    app: IdeApp,
    directory: tempfile::TempDir,
    source: PathBuf,
    index: usize,
}
struct Captured {
    id: u64,
    directory: PathBuf,
    output: PathBuf,
    format: String,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for session in &mut self.app.run_panel.sessions {
            session.stop();
        }
        // Keep App/artifact directories and source authority alive through real
        // process/pipe retirement even when an assertion unwinds. ROOT's outer
        // actor retains emergency authority; no local deadline abandons a child.
        loop {
            self.app.run_panel.poll();
            if self
                .app
                .run_panel
                .sessions
                .iter()
                .all(|session| !session.owns_process())
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("explicit {name} required"))
}
impl Fixture {
    fn admitted() -> Self {
        assert_eq!(required("RUSTPY_PANDOC_ADMITTED"), "1");
        assert!(
            std::env::var_os("RUSTPY_TRUST_ALL").is_none(),
            "ambient trust bypass must be absent"
        );
        let temporary = PathBuf::from(required("TMPDIR"));
        assert!(temporary.is_absolute() && temporary.is_dir());
        let temporary = dunce::canonicalize(&temporary).unwrap();
        // Markdown snapshots use Settings::config_dir(). App settings/cache/log
        // authority must therefore be isolated before constructing the App.
        for name in ["RUSTPY_CONFIG_DIR", "RUSTPY_SYSTEM_DIR", "RUSTPY_LOG_DIR"] {
            let path = PathBuf::from(required(name));
            assert!(path.is_absolute() && path.starts_with(&temporary));
            std::fs::create_dir_all(&path).unwrap();
            assert!(dunce::canonicalize(&path).unwrap().starts_with(&temporary));
        }
        let pandoc = PathBuf::from(required("RUSTPY_TEST_PANDOC"));
        assert!(pandoc.is_absolute() && pandoc.is_file());
        let expected = required("RUSTPY_TEST_PANDOC_SHA256");
        assert!(expected.len() == 64 && expected.bytes().all(|b| b.is_ascii_hexdigit()));
        let actual: String = rustpy_net::x509::sha256(&std::fs::read(&pandoc).unwrap())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(actual, expected, "admitted genuine Pandoc bytes changed");
        let directory = tempfile::Builder::new()
            .prefix("markdown export π ")
            .tempdir_in(&temporary)
            .unwrap();
        let source = directory.path().join("source π.md");
        std::fs::write(&source, "# Disk revision must not replace editor content\n").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(directory.path());
        settings.markdown.pandoc = pandoc.to_string_lossy().into_owned();
        let mut app = IdeApp::new(settings, Some(directory.path().into()));
        app.open_file(&source);
        let index = app.active.unwrap();
        assert!(!app.project_safe_mode());
        assert!(app.builtin_plugin_enabled("markdown"));
        app.tabs[index]
            .state
            .doc
            .set_text(
                "# Captured heading π\n\nEditor revision42 **bold marker** and café 雪.\n\n| Key | Value |\n| --- | --- |\n| Unicode café | 42 |\n\n[Owned link](https://example.invalid/owned-document)\n",
                &[],
                &[],
            )
            .unwrap();
        Self {
            app,
            directory,
            source,
            index,
        }
    }
    fn ids(&self) -> Vec<u64> {
        self.app.run_panel.sessions.iter().map(|s| s.id).collect()
    }
    fn export(&mut self, destination: &Path, overwrite: bool) -> Option<Captured> {
        self.export_format(destination, overwrite, "docx")
    }
    fn export_format(
        &mut self,
        destination: &Path,
        overwrite: bool,
        format: &str,
    ) -> Option<Captured> {
        let source = self.app.tabs[self.index].state.doc.text();
        let version = self.app.tabs[self.index].state.doc.version();
        let mut workspace = std::mem::take(&mut self.app.markdown_workspace);
        assert!(workspace.artifacts.is_empty());
        workspace.export_path = destination.to_string_lossy().into_owned();
        workspace.overwrite = overwrite;
        workspace.message.clear();
        // Same private production handler invoked by the actual Export DOCX UI
        // button, with no alternate launch plan or synthetic producer result.
        self.app.markdown_export(
            self.index,
            &source,
            Some(&self.source),
            format,
            &mut workspace,
        );
        // Transfer the newly launched snapshot back to the guarded App before
        // any assertion can unwind and remove files still used by Pandoc.
        self.app.markdown_workspace = workspace;
        self.app
            .markdown_workspace
            .artifacts
            .last()
            .map(|artifact| {
                assert_eq!(artifact.version, version);
                assert_eq!(artifact.file.as_ref(), Some(&self.source));
                assert_eq!(artifact.root, self.directory.path());
                let output = artifact.output.as_ref().unwrap();
                assert_eq!(output.destination, destination);
                let session = self
                    .app
                    .run_panel
                    .sessions
                    .iter()
                    .find(|s| s.id == artifact.id)
                    .unwrap();
                assert_eq!(session.name, format!("Markdown: export {format}"));
                let cfg = session.config.as_ref().unwrap();
                let RunConfigKind::Program { program, args } = &cfg.kind else {
                    panic!("genuine export must retain its captured Program configuration")
                };
                assert_eq!(program, &PathBuf::from(&self.app.settings.markdown.pandoc));
                assert_eq!(cfg.working_dir.as_deref(), self.source.parent());
                assert_eq!(args[0], "--from=gfm+tex_math_dollars");
                assert_eq!(std::fs::read_to_string(&args[1]).unwrap(), source);
                let at = args.iter().position(|a| a == "--output").unwrap();
                assert_eq!(Path::new(&args[at + 1]), output.temporary);
                assert_ne!(output.temporary, destination);
                if format == "pdf" && !self.app.settings.markdown.pdf_engine.is_empty() {
                    assert!(args.contains(&format!(
                        "--pdf-engine={}",
                        self.app.settings.markdown.pdf_engine
                    )));
                } else {
                    assert!(!args.iter().any(|arg| arg.starts_with("--pdf-engine=")));
                }
                Captured {
                    id: artifact.id,
                    directory: artifact.directory.path().into(),
                    output: output.temporary.clone(),
                    format: format.into(),
                }
            })
    }
    fn finish_producer(&mut self, captured: &Captured) {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            // Deliberately leave the real completion queued for the production
            // publisher, so stale/trust/destination changes are deterministic
            // without a fake Pandoc or artificial provider rendezvous.
            self.app.run_panel.poll();
            let session = self
                .app
                .run_panel
                .sessions
                .iter()
                .find(|s| s.id == captured.id)
                .unwrap();
            if !session.owns_process() {
                let output: String = session
                    .output
                    .iter()
                    .map(|chunk| chunk.text.as_str())
                    .collect();
                assert_eq!(
                    session.status,
                    rustpy_run::SessionStatus::Exited(0),
                    "real Pandoc export failed: {output}"
                );
                assert!(captured.output.is_file());
                let bytes = std::fs::read(&captured.output).unwrap();
                if captured.format == "docx" {
                    assert_docx(&bytes);
                } else {
                    assert!(bytes.starts_with(b"%PDF-") && bytes.len() < 16 * 1024 * 1024);
                }
                return;
            }
            assert!(
                Instant::now() < deadline,
                "actual Pandoc process did not retire"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn publish(&mut self, captured: &Captured) {
        self.app.poll_markdown_jobs();
        assert!(self.app.markdown_workspace.artifacts.is_empty());
        assert!(
            !captured.directory.exists(),
            "retired artifact snapshot must be released"
        );
        assert!(self
            .app
            .run_panel
            .sessions
            .iter()
            .any(|s| s.id == captured.id && !s.owns_process()));
    }
}

// Decode only the small genuine DOCX ZIP entries needed by the acceptance
// oracle, using existing flate2. Central-directory sizes handle ZIP data
// descriptors correctly; no new archive dependency or external unzip process.
fn zip_entry(bytes: &[u8], wanted: &str) -> Vec<u8> {
    fn u16_at(b: &[u8], at: usize) -> usize {
        u16::from_le_bytes(b[at..at + 2].try_into().unwrap()).into()
    }
    fn u32_at(b: &[u8], at: usize) -> usize {
        u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize
    }
    assert!(bytes.starts_with(b"PK\x03\x04") && bytes.len() < 16 * 1024 * 1024);
    let end = (0..=bytes.len().checked_sub(22).unwrap())
        .rev()
        .find(|&at| {
            bytes[at..].starts_with(b"PK\x05\x06")
                && at + 22 + u16_at(bytes, at + 20) == bytes.len()
        })
        .expect("genuine DOCX end-of-central-directory signature");
    let count = u16_at(bytes, end + 10);
    assert!(count > 0 && count < 512);
    let mut at = u32_at(bytes, end + 16);
    for _ in 0..count {
        assert!(bytes[at..].starts_with(b"PK\x01\x02"));
        let size = u32_at(bytes, at + 20);
        let plain_size = u32_at(bytes, at + 24);
        let name_len = u16_at(bytes, at + 28);
        let name = std::str::from_utf8(&bytes[at + 46..at + 46 + name_len]).unwrap();
        if name == wanted {
            assert_eq!(u16_at(bytes, at + 8) & 1, 0, "DOCX is not encrypted");
            assert!(size <= 8 * 1024 * 1024 && plain_size <= 8 * 1024 * 1024);
            let local = u32_at(bytes, at + 42);
            assert!(bytes[local..].starts_with(b"PK\x03\x04"));
            let data = local + 30 + u16_at(bytes, local + 26) + u16_at(bytes, local + 28);
            let compressed = &bytes[data..data + size];
            let decoded = match u16_at(bytes, at + 10) {
                0 => compressed.to_vec(),
                8 => {
                    let mut decoded = Vec::new();
                    flate2::read::DeflateDecoder::new(compressed)
                        .take(8 * 1024 * 1024 + 1)
                        .read_to_end(&mut decoded)
                        .unwrap();
                    decoded
                }
                method => panic!("unsupported genuine DOCX compression method {method}"),
            };
            assert_eq!(decoded.len(), plain_size);
            return decoded;
        }
        at += 46 + name_len + u16_at(bytes, at + 30) + u16_at(bytes, at + 32);
    }
    panic!("genuine DOCX missing {wanted}")
}
fn assert_docx(bytes: &[u8]) {
    let types = zip_entry(bytes, "[Content_Types].xml");
    let types = std::str::from_utf8(&types).unwrap();
    assert!(types.contains("wordprocessingml.document.main+xml"));
    let document = zip_entry(bytes, "word/document.xml");
    let document = roxmltree::Document::parse(std::str::from_utf8(&document).unwrap()).unwrap();
    let text: String = document
        .descendants()
        .filter_map(|n| n.text().filter(|_| n.is_text()))
        .collect();
    for expected in [
        "Captured heading π",
        "Editor revision42",
        "bold marker",
        "café 雪",
        "Unicode café",
        "42",
        "Owned link",
    ] {
        assert!(
            text.contains(expected),
            "actual DOCX content missing {expected}"
        );
    }
    assert!(!text.contains("Disk revision must not replace"));
    assert!(
        document.descendants().any(|n| n.has_tag_name((
            "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
            "tbl"
        ))),
        "actual DOCX table expected"
    );
    let relationships = zip_entry(bytes, "word/_rels/document.xml.rels");
    let relationships =
        roxmltree::Document::parse(std::str::from_utf8(&relationships).unwrap()).unwrap();
    assert!(relationships
        .descendants()
        .any(|node| node.attribute("Target") == Some("https://example.invalid/owned-document")));
}

#[test]
#[ignore = "ROOT-admitted absolute genuine Pandoc and isolated config/system/log dirs; DOCX producer acceptance"]
fn configured_pandoc_docx_export_preserves_snapshot_and_publication_authority() {
    let mut fixture = Fixture::admitted();
    // A configured engine must never leak into DOCX arguments. Reusing the
    // admitted executable spelling avoids introducing any extra tool here.
    fixture.app.settings.markdown.pdf_engine = fixture.app.settings.markdown.pandoc.clone();
    let destination = fixture.directory.path().join("actual output π.docx");
    let captured = fixture.export(&destination, false).unwrap();
    assert!(!destination.exists());
    fixture.finish_producer(&captured);
    assert!(
        !destination.exists(),
        "producer must write only its owned staging output"
    );
    fixture.publish(&captured);
    assert_docx(&std::fs::read(&destination).unwrap());
    assert!(fixture
        .app
        .markdown_workspace
        .message
        .starts_with("Export completed:"));
    assert_eq!(
        std::fs::read_to_string(&fixture.source).unwrap(),
        "# Disk revision must not replace editor content\n"
    );

    let ids = fixture.ids();
    assert!(fixture.export(&destination, false).is_none());
    assert_eq!(
        fixture.ids(),
        ids,
        "overwrite refusal must not launch Pandoc"
    );
    assert!(fixture
        .app
        .markdown_workspace
        .message
        .contains("enable overwrite"));

    let ids = fixture.ids();
    let source_path = fixture.source.clone();
    assert!(fixture.export(&source_path, true).is_none());
    assert_eq!(
        fixture.ids(),
        ids,
        "source overwrite refusal must not launch Pandoc"
    );
    assert_eq!(
        std::fs::read_to_string(&fixture.source).unwrap(),
        "# Disk revision must not replace editor content\n"
    );

    let stale = fixture.directory.path().join("stale.docx");
    let captured = fixture.export(&stale, false).unwrap();
    fixture.finish_producer(&captured);
    fixture.app.tabs[fixture.index]
        .state
        .doc
        .set_text("# Changed revision\n", &[], &[])
        .unwrap();
    fixture.publish(&captured);
    assert!(!stale.exists(), "stale source may never publish");
    assert!(fixture
        .app
        .markdown_workspace
        .message
        .contains("not published"));
    // Restore text via the normal document edit API; version authority remains
    // different even when content returns to the same source bytes.
    fixture.app.tabs[fixture.index].state.doc.undo().unwrap();

    let guarded = fixture.directory.path().join("external write.docx");
    std::fs::write(&guarded, "owned baseline").unwrap();
    let captured = fixture.export(&guarded, true).unwrap();
    fixture.finish_producer(&captured);
    std::fs::write(&guarded, "external revision43").unwrap();
    fixture.publish(&captured);
    assert_eq!(
        std::fs::read_to_string(&guarded).unwrap(),
        "external revision43"
    );
    assert!(fixture
        .app
        .markdown_workspace
        .message
        .contains("destination changed"));

    let revoked = fixture.directory.path().join("revoked.docx");
    let captured = fixture.export(&revoked, false).unwrap();
    fixture.finish_producer(&captured);
    fixture.app.settings.trusted_projects.clear();
    assert!(!fixture.app.safe_mode && fixture.app.project_safe_mode());
    fixture.publish(&captured);
    assert!(
        !revoked.exists(),
        "revoked trust cannot publish completed project output"
    );
    let ids = fixture.ids();
    assert!(fixture.export(&revoked, false).is_none());
    assert_eq!(
        fixture.ids(),
        ids,
        "untrusted action must not launch Pandoc"
    );
    assert!(fixture
        .app
        .markdown_workspace
        .message
        .contains("Trust the project"));
}

fn admitted_pdf_tool(name: &str) -> PathBuf {
    let path = PathBuf::from(required(name));
    assert!(path.is_absolute() && path.is_file());
    let expected = required(&format!("{name}_SHA256"));
    let actual: String = rustpy_net::x509::sha256(&std::fs::read(&path).unwrap())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(actual, expected, "admitted PDF tool bytes changed");
    path
}
fn read_pdf(tool: &Path, args: &[&std::ffi::OsStr]) -> String {
    let mut command = std::process::Command::new(tool);
    command.args(args);
    let (out, _) = rustpy_project::native_skeleton::supervised_tool(
        &mut command,
        &rustpy_core::CancellationToken::new(),
        Duration::from_secs(30),
        1024 * 1024,
    )
    .expect("genuine admitted PDF reader must succeed");
    String::from_utf8(out).unwrap()
}

#[test]
#[ignore = "ROOT-admitted genuine Pandoc, WeasyPrint, pdfinfo and pdftotext with pinned bytes/private profile"]
fn configured_pandoc_weasyprint_pdf_exports_unsaved_content_and_rejects_stale_publication() {
    let mut fixture = Fixture::admitted();
    assert!(
        fixture.app.settings.markdown.pdf_engine.is_empty(),
        "default must preserve Pandoc's engine choice"
    );
    let engine = admitted_pdf_tool("RUSTPY_TEST_WEASYPRINT");
    let info = admitted_pdf_tool("RUSTPY_TEST_PDFINFO");
    let text = admitted_pdf_tool("RUSTPY_TEST_PDFTOTEXT");
    fixture.app.settings.markdown.pdf_engine = engine.to_str().unwrap().into();
    let destination = fixture.directory.path().join("actual output π.pdf");
    let captured = fixture.export_format(&destination, false, "pdf").unwrap();
    fixture.finish_producer(&captured);
    assert!(!destination.exists());
    fixture.publish(&captured);
    let bytes = std::fs::read(&destination).unwrap();
    assert!(bytes.starts_with(b"%PDF-") && bytes.len() < 16 * 1024 * 1024);
    let metadata = read_pdf(&info, &[destination.as_os_str()]);
    assert!(
        metadata.lines().any(|line| line
            .strip_prefix("Pages:")
            .and_then(|pages| pages.trim().parse::<usize>().ok())
            .is_some_and(|pages| pages > 0)),
        "real PDF must contain pages: {metadata}"
    );
    let extracted = read_pdf(
        &text,
        &[
            std::ffi::OsStr::new("-layout"),
            destination.as_os_str(),
            std::ffi::OsStr::new("-"),
        ],
    );
    for expected in [
        "Captured heading π",
        "Editor revision42",
        "bold marker",
        "café",
        "Unicode café",
        "42",
        "Owned link",
    ] {
        assert!(
            extracted.contains(expected),
            "actual PDF content missing {expected}: {extracted}"
        );
    }
    assert!(!extracted.contains("Disk revision must not replace"));
    // Inspect real link annotations through Poppler, including compressed PDF
    // objects; byte searching would incorrectly assume an uncompressed writer.
    let urls = read_pdf(
        &info,
        &[std::ffi::OsStr::new("-url"), destination.as_os_str()],
    );
    assert!(urls.contains("https://example.invalid/owned-document"));
    assert_eq!(
        std::fs::read_to_string(&fixture.source).unwrap(),
        "# Disk revision must not replace editor content\n"
    );
    let stale = fixture.directory.path().join("stale.pdf");
    let captured = fixture.export_format(&stale, false, "pdf").unwrap();
    fixture.finish_producer(&captured);
    fixture.app.tabs[fixture.index]
        .state
        .doc
        .set_text("# Changed revision\n", &[], &[])
        .unwrap();
    fixture.publish(&captured);
    assert!(!stale.exists());
    assert!(fixture
        .app
        .markdown_workspace
        .message
        .contains("not published"));
}
