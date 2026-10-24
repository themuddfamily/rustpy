//! In-process protocol tests: a client drives the server over
//! `Connection::memory()` against a project in a temporary directory.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::Notification as _;
use lsp_types::request::Request as _;
use lsp_types::*;
use serde_json::{json, Value};

use crate::{run, ServerOptions};

const TIMEOUT: Duration = Duration::from_secs(60);

struct Client {
    conn: Connection,
    server: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
    next_id: i32,
    /// Notifications received while waiting for responses.
    notifications: Vec<Notification>,
    pub init: InitializeResult,
    dir: tempfile::TempDir,
}

fn test_settings() -> rustpy_settings::Settings {
    let mut s = rustpy_settings::Settings::default();
    // Prose checks depend on the bundled dictionary; the tests look at code inspections.
    s.inspections.spelling = false;
    s.inspections.grammar = false;
    s
}

fn client_caps() -> ClientCapabilities {
    serde_json::from_value(json!({
        "general": { "positionEncodings": ["utf-16"] },
        "textDocument": {
            "hover": { "contentFormat": ["markdown", "plaintext"] },
            "completion": { "completionItem": { "snippetSupport": true, "documentationFormat": ["markdown"], "insertTextModeSupport": { "valueSet": [1, 2] } } },
            "signatureHelp": { "signatureInformation": { "documentationFormat": ["markdown"], "parameterInformation": { "labelOffsetSupport": true }, "activeParameterSupport": true } },
            "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
            "publishDiagnostics": { "tagSupport": { "valueSet": [1, 2] }, "versionSupport": true },
            "foldingRange": { "lineFoldingOnly": true },
            "codeAction": { "codeActionLiteralSupport": { "codeActionKind": { "valueSet": ["quickfix", "refactor", "source"] } } }
        },
        "workspace": { "workspaceEdit": { "documentChanges": true }, "workspaceFolders": true }
    }))
    .unwrap()
}

impl Client {
    /// Starts a server on a project made of `files` (relative path, text).
    fn start(files: &[(&str, &str)]) -> Client {
        Self::start_with(files, client_caps(), json!({}))
    }

    fn start_with(
        files: &[(&str, &str)],
        caps: ClientCapabilities,
        extra_options: Value,
    ) -> Client {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for (rel, text) in files {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        let (server_conn, conn) = Connection::memory();
        let options = ServerOptions {
            settings: Some(test_settings()),
            stub_dir: Some(root.join(".stubs")),
        };
        let server = std::thread::spawn(move || run(server_conn, options));
        let mut init_options =
            json!({ "interpreter": "none", "diagnosticsDelay": 20, "autoImportCompletions": true });
        crate::config::merge(&mut init_options, &extra_options);
        let mut client = Client {
            conn,
            server: Some(server),
            next_id: 0,
            notifications: Vec::new(),
            init: InitializeResult::default(),
            dir,
        };
        let params = json!({
            "processId": null,
            "capabilities": serde_json::to_value(caps).unwrap(),
            "workspaceFolders": [{ "uri": uri(&root).as_str(), "name": "project" }],
            "initializationOptions": init_options,
        });
        let result = client.raw_request(request::Initialize::METHOD, params);
        client.init = serde_json::from_value(result.expect("initialize succeeds")).unwrap();
        client.notify::<notification::Initialized>(InitializedParams {});
        client
    }

    fn root(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }

    fn uri(&self, rel: &str) -> Uri {
        uri(&self.root().join(rel))
    }

    fn notify<N: notification::Notification>(&self, params: N::Params) {
        self.conn
            .sender
            .send(Message::Notification(Notification::new(
                N::METHOD.into(),
                params,
            )))
            .unwrap();
    }

    fn send_request(&mut self, method: &str, params: Value) -> RequestId {
        self.next_id += 1;
        let id = RequestId::from(self.next_id);
        self.conn
            .sender
            .send(Message::Request(Request {
                id: id.clone(),
                method: method.into(),
                params,
            }))
            .unwrap();
        id
    }

    /// Waits for the response to `id`, keeping notifications for later.
    fn response(&mut self, id: &RequestId) -> Response {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self
                .conn
                .receiver
                .recv_timeout(left)
                .expect("server answered in time")
            {
                Message::Response(r) if &r.id == id => return r,
                Message::Response(r) => panic!("unexpected response {r:?}"),
                Message::Notification(n) => self.notifications.push(n),
                Message::Request(r) => {
                    // Server-to-client requests (registrations): accept them.
                    self.conn
                        .sender
                        .send(Message::Response(Response::new_ok(r.id, ())))
                        .unwrap();
                }
            }
        }
    }

    fn raw_request(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<Value, lsp_server::ResponseError> {
        let id = self.send_request(method, params);
        self.response(&id).response_result
    }

    fn request<R: request::Request>(&mut self, params: R::Params) -> R::Result {
        let result = self.raw_request(R::METHOD, serde_json::to_value(params).unwrap());
        serde_json::from_value(
            result.unwrap_or_else(|e| panic!("{} failed: {}", R::METHOD, e.message)),
        )
        .unwrap()
    }

    fn open(&self, rel: &str) -> Uri {
        let text = std::fs::read_to_string(self.root().join(rel)).unwrap();
        self.open_text(rel, &text)
    }

    fn open_text(&self, rel: &str, text: &str) -> Uri {
        let uri = self.uri(rel);
        self.notify::<notification::DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: "python".into(),
                version: 1,
                text: text.into(),
            },
        });
        uri
    }

    /// The next diagnostics published for `uri` (with `version`, when given).
    fn diagnostics(&mut self, uri: &Uri, version: Option<i32>) -> Vec<Diagnostic> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(i) = self.notifications.iter().position(|n| {
                n.method == notification::PublishDiagnostics::METHOD
                    && serde_json::from_value::<PublishDiagnosticsParams>(n.params.clone())
                        .is_ok_and(|p| &p.uri == uri && (version.is_none() || p.version == version))
            }) {
                let n = self.notifications.remove(i);
                return serde_json::from_value::<PublishDiagnosticsParams>(n.params)
                    .unwrap()
                    .diagnostics;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self
                .conn
                .receiver
                .recv_timeout(left)
                .expect("diagnostics published in time")
            {
                Message::Notification(n) => self.notifications.push(n),
                Message::Request(r) => self
                    .conn
                    .sender
                    .send(Message::Response(Response::new_ok(r.id, ())))
                    .unwrap(),
                Message::Response(r) => panic!("unexpected response {r:?}"),
            }
        }
    }

    fn shutdown(mut self) {
        let _: () = self.request::<request::Shutdown>(());
        self.notify::<notification::Exit>(());
        let result = self.server.take().unwrap().join().expect("server thread");
        assert!(result.is_ok(), "server exits cleanly: {result:?}");
    }
}

fn uri(path: &Path) -> Uri {
    Uri::from_str(url::Url::from_file_path(path).unwrap().as_str()).unwrap()
}

fn path_of(uri: &Uri) -> PathBuf {
    url::Url::parse(uri.as_str())
        .unwrap()
        .to_file_path()
        .unwrap()
}

fn pos(line: u32, character: u32) -> Position {
    Position::new(line, character)
}

/// Position of the `n`th occurrence of `needle` in `text`.
fn find(text: &str, needle: &str, n: usize) -> Position {
    let off = text
        .match_indices(needle)
        .nth(n)
        .unwrap_or_else(|| panic!("{needle:?} #{n} not in text"))
        .0;
    let line = text[..off].matches('\n').count() as u32;
    let col = text[..off]
        .rsplit('\n')
        .next()
        .unwrap()
        .encode_utf16()
        .count() as u32;
    pos(line, col)
}

fn tdp(uri: &Uri, p: Position) -> TextDocumentPositionParams {
    TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        position: p,
    }
}

/// Applies LSP text edits to `text` (UTF-16 positions).
fn apply(text: &str, edits: &[TextEdit]) -> String {
    let index = crate::line_index::LineIndex::new(text);
    let mut edits: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|e| {
            (
                usize::from(index.offset(e.range.start, crate::line_index::Encoding::Utf16)),
                usize::from(index.offset(e.range.end, crate::line_index::Encoding::Utf16)),
                e.new_text.as_str(),
            )
        })
        .collect();
    edits.sort_by_key(|e| std::cmp::Reverse(e.0));
    let mut out = text.to_string();
    for (s, e, t) in edits {
        out.replace_range(s..e, t);
    }
    out
}

/// The edits for `uri` in a workspace edit.
fn edits_for(edit: &WorkspaceEdit, uri: &Uri) -> Vec<TextEdit> {
    match &edit.document_changes {
        Some(DocumentChanges::Edits(docs)) => docs
            .iter()
            .filter(|d| &d.text_document.uri == uri)
            .flat_map(|d| {
                d.edits.iter().map(|e| match e {
                    OneOf::Left(e) => e.clone(),
                    OneOf::Right(a) => a.text_edit.clone(),
                })
            })
            .collect(),
        _ => edit
            .changes
            .as_ref()
            .and_then(|c| c.get(uri))
            .cloned()
            .unwrap_or_default(),
    }
}

const UTIL: &str = "def helper(name: str) -> str:\n    \"\"\"Greets someone by name.\"\"\"\n    return 'hi ' + name\n\n\nclass Greeter:\n    def greet(self, who):\n        return helper(who)\n";
const MAIN: &str = "import os\nfrom util import helper, Greeter\n\n\ndef main():\n    value = helper('x')\n    print(value, os.sep)\n    return Greeter().greet(value)\n";

#[test]
fn initialize_announces_capabilities() {
    let c = Client::start(&[("main.py", "x = 1\n")]);
    let caps = &c.init.capabilities;
    assert_eq!(
        c.init.server_info.as_ref().map(|s| s.name.as_str()),
        Some("rustpy")
    );
    assert_eq!(caps.position_encoding, Some(PositionEncodingKind::UTF16));
    assert!(matches!(
        caps.text_document_sync,
        Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                change: Some(TextDocumentSyncKind::INCREMENTAL),
                ..
            }
        ))
    ));
    assert!(
        caps.hover_provider.is_some()
            && caps
                .completion_provider
                .as_ref()
                .is_some_and(|c| c.resolve_provider == Some(true))
    );
    assert!(matches!(
        caps.rename_provider,
        Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            ..
        }))
    ));
    assert!(
        caps.semantic_tokens_provider.is_some()
            && caps.inlay_hint_provider.is_some()
            && caps.call_hierarchy_provider.is_some()
    );
    assert!(
        caps.workspace
            .as_ref()
            .and_then(|w| w.workspace_folders.as_ref())
            .and_then(|f| f.supported)
            == Some(true)
    );
    c.shutdown();
}

#[test]
fn utf8_is_negotiated_when_offered() {
    let mut caps = client_caps();
    caps.general = Some(GeneralClientCapabilities {
        position_encodings: Some(vec![
            PositionEncodingKind::UTF16,
            PositionEncodingKind::UTF8,
        ]),
        ..Default::default()
    });
    let mut c = Client::start_with(&[("m.py", "s = 'é'; t = s\n")], caps, json!({}));
    assert_eq!(
        c.init.capabilities.position_encoding,
        Some(PositionEncodingKind::UTF8)
    );
    let uri = c.open("m.py");
    // `s` after `t = ` is at byte column 14 (the é takes two bytes).
    let hl: Option<Vec<DocumentHighlight>> =
        c.request::<request::DocumentHighlightRequest>(DocumentHighlightParams {
            text_document_position_params: tdp(&uri, pos(0, 14)),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
    let ranges: Vec<Range> = hl.unwrap().into_iter().map(|h| h.range).collect();
    assert!(
        ranges.contains(&Range::new(pos(0, 0), pos(0, 1)))
            && ranges.contains(&Range::new(pos(0, 14), pos(0, 15))),
        "{ranges:?}"
    );
    c.shutdown();
}

#[test]
fn did_open_publishes_inspection_diagnostics() {
    let src = "import sys\n\n\ndef f():\n    return undefined_name\n\n\nx = (\n";
    let mut c = Client::start(&[("bad.py", src)]);
    let uri = c.open("bad.py");
    let diags = c.diagnostics(&uri, Some(1));
    let code = |d: &Diagnostic| match &d.code {
        Some(NumberOrString::String(s)) => s.clone(),
        _ => String::new(),
    };
    let unresolved = diags
        .iter()
        .find(|d| code(d) == "PyUnresolvedReferences")
        .expect("unresolved reference reported");
    assert_eq!(unresolved.severity, Some(DiagnosticSeverity::ERROR));
    assert_eq!(unresolved.message, "Unresolved reference 'undefined_name'");
    assert_eq!(unresolved.range, Range::new(pos(4, 11), pos(4, 25)));
    assert_eq!(unresolved.source.as_deref(), Some("RustPy"));
    let unused = diags
        .iter()
        .find(|d| code(d) == "PyUnusedImports")
        .expect("unused import reported");
    assert_eq!(unused.severity, Some(DiagnosticSeverity::WARNING));
    assert_eq!(
        unused.tags.as_deref(),
        Some(&[DiagnosticTag::UNNECESSARY][..])
    );
    assert!(
        diags
            .iter()
            .any(|d| code(d) == "syntax" && d.severity == Some(DiagnosticSeverity::ERROR)),
        "{diags:?}"
    );
    c.shutdown();
}

#[test]
fn did_change_applies_incremental_edits() {
    let mut c = Client::start(&[("m.py", "def f():\n    return valeu\n")]);
    let uri = c.open("m.py");
    let diags = c.diagnostics(&uri, Some(1));
    assert!(
        diags
            .iter()
            .any(|d| d.message == "Unresolved reference 'valeu'"),
        "{diags:?}"
    );
    // Define the name above and fix the typo, as two incremental changes.
    let changes = vec![
        TextDocumentContentChangeEvent {
            range: Some(Range::new(pos(1, 11), pos(1, 16))),
            range_length: None,
            text: "value".into(),
        },
        TextDocumentContentChangeEvent {
            range: Some(Range::new(pos(0, 0), pos(0, 0))),
            range_length: None,
            text: "value = 1\n\n\n".into(),
        },
    ];
    c.notify::<notification::DidChangeTextDocument>(DidChangeTextDocumentParams {
        text_document: VersionedTextDocumentIdentifier {
            uri: uri.clone(),
            version: 2,
        },
        content_changes: changes,
    });
    let diags = c.diagnostics(&uri, Some(2));
    assert!(
        !diags
            .iter()
            .any(|d| d.message.starts_with("Unresolved reference")),
        "{diags:?}"
    );
    // Requests see the new text right away.
    let def: Option<GotoDefinitionResponse> =
        c.request::<request::GotoDefinition>(GotoDefinitionParams {
            text_document_position_params: tdp(&uri, pos(4, 13)),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
    let Some(GotoDefinitionResponse::Array(locs)) = def else {
        panic!("definition expected")
    };
    assert_eq!(locs[0].range, Range::new(pos(0, 0), pos(0, 5)));
    // A full-text change (no range) replaces the document.
    c.notify::<notification::DidChangeTextDocument>(DidChangeTextDocumentParams {
        text_document: VersionedTextDocumentIdentifier {
            uri: uri.clone(),
            version: 3,
        },
        content_changes: vec![TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: "print(missing)\n".into(),
        }],
    });
    let diags = c.diagnostics(&uri, Some(3));
    assert!(
        diags
            .iter()
            .any(|d| d.message == "Unresolved reference 'missing'"),
        "{diags:?}"
    );
    c.shutdown();
}

#[test]
fn completion_offers_members_names_and_documentation() {
    let src = "import os\n\n\ndef compute_total(items):\n    \"\"\"Adds the items up.\"\"\"\n    return sum(items)\n\n\nos.pa\ncompute_t\n";
    let mut c = Client::start(&[("m.py", src)]);
    let uri = c.open("m.py");
    let complete = |c: &mut Client, p: Position| -> CompletionList {
        let r: Option<CompletionResponse> = c.request::<request::Completion>(CompletionParams {
            text_document_position: tdp(&uri, p),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        });
        match r.unwrap() {
            CompletionResponse::List(l) => l,
            CompletionResponse::Array(a) => CompletionList {
                is_incomplete: false,
                items: a,
            },
        }
    };
    // Members of a module after the dot.
    let list = complete(&mut c, pos(8, 5));
    let path = list
        .items
        .iter()
        .find(|i| i.label == "path")
        .expect("os.path offered");
    assert_eq!(path.kind, Some(CompletionItemKind::MODULE));
    let Some(CompletionTextEdit::Edit(edit)) = &path.text_edit else {
        panic!("text edit expected")
    };
    assert_eq!(edit.range, Range::new(pos(8, 3), pos(8, 5)));
    assert!(
        list.items.iter().all(|i| i.label != "compute_total"),
        "only members after a dot"
    );
    // Names in scope, with the call's parentheses as a snippet.
    let list = complete(&mut c, pos(9, 9));
    let item = list
        .items
        .iter()
        .find(|i| i.label == "compute_total")
        .expect("function offered")
        .clone();
    assert_eq!(item.kind, Some(CompletionItemKind::FUNCTION));
    let Some(CompletionTextEdit::Edit(edit)) = &item.text_edit else {
        panic!("text edit expected")
    };
    assert_eq!(edit.new_text, "compute_total($0)");
    assert_eq!(item.insert_text_format, Some(InsertTextFormat::SNIPPET));
    // Documentation comes with completionItem/resolve.
    let resolved: CompletionItem = c.request::<request::ResolveCompletionItem>(item);
    let Some(Documentation::MarkupContent(doc)) = resolved.documentation else {
        panic!("documentation expected")
    };
    assert!(
        doc.value.contains("def compute_total(items)"),
        "{}",
        doc.value
    );
    assert!(doc.value.contains("Adds the items up."), "{}", doc.value);
    c.shutdown();
}

#[test]
fn completion_inside_a_name_can_replace_its_rest() {
    let mut caps = client_caps();
    if let Some(item) = caps
        .text_document
        .as_mut()
        .and_then(|t| t.completion.as_mut())
        .and_then(|c| c.completion_item.as_mut())
    {
        item.insert_replace_support = Some(true);
    }
    let mut c = Client::start_with(
        &[("m.py", "def compute_total():\n    pass\n\n\ncompute_tal\n")],
        caps,
        json!({}),
    );
    let uri = c.open("m.py");
    let r: Option<CompletionResponse> = c.request::<request::Completion>(CompletionParams {
        text_document_position: tdp(&uri, pos(4, 9)),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
        context: None,
    });
    let Some(CompletionResponse::List(list)) = r else {
        panic!("list expected")
    };
    let item = list
        .items
        .iter()
        .find(|i| i.label == "compute_total")
        .expect("function offered");
    let Some(CompletionTextEdit::InsertAndReplace(edit)) = &item.text_edit else {
        panic!("insert/replace edit expected: {:?}", item.text_edit)
    };
    assert_eq!(edit.insert, Range::new(pos(4, 0), pos(4, 9)));
    assert_eq!(edit.replace, Range::new(pos(4, 0), pos(4, 11)));
    c.shutdown();
}

#[test]
fn completion_adds_imports_and_expands_templates() {
    let mut c = Client::start(&[
        ("util.py", UTIL),
        ("m.py", "def run():\n    Greet\n    defaultd\n"),
    ]);
    let uri = c.open("m.py");
    // The symbol index is built in the background; ask until project names show up.
    let deadline = Instant::now() + TIMEOUT;
    let greeter = loop {
        let r: Option<CompletionResponse> = c.request::<request::Completion>(CompletionParams {
            text_document_position: tdp(&uri, pos(1, 9)),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        });
        let Some(CompletionResponse::List(list)) = r else {
            panic!("list expected")
        };
        if let Some(i) = list.items.into_iter().find(|i| i.label == "Greeter") {
            break i;
        }
        assert!(Instant::now() < deadline, "project symbol offered");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(
        greeter
            .additional_text_edits
            .as_ref()
            .map(|e| e[0].new_text.as_str()),
        Some("from util import Greeter\n")
    );
    let r: Option<CompletionResponse> = c.request::<request::Completion>(CompletionParams {
        text_document_position: tdp(&uri, pos(2, 12)),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
        context: None,
    });
    let Some(CompletionResponse::List(list)) = r else {
        panic!("list expected")
    };
    let dd = list
        .items
        .iter()
        .find(|i| i.label == "defaultdict")
        .expect("stdlib name offered");
    assert_eq!(
        dd.additional_text_edits
            .as_ref()
            .map(|e| e[0].new_text.as_str()),
        Some("from collections import defaultdict\n")
    );
    // A live template as a snippet: `main` → `if __name__ == '__main__':`.
    let doc = c.open_text("t.py", "mai\n");
    let r: Option<CompletionResponse> = c.request::<request::Completion>(CompletionParams {
        text_document_position: tdp(&doc, pos(0, 3)),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
        context: None,
    });
    let Some(CompletionResponse::List(list)) = r else {
        panic!("list expected")
    };
    let main = list
        .items
        .iter()
        .find(|i| i.label == "main" && i.kind == Some(CompletionItemKind::SNIPPET))
        .expect("live template offered");
    let Some(CompletionTextEdit::Edit(edit)) = &main.text_edit else {
        panic!("text edit expected")
    };
    assert!(
        edit.new_text.starts_with("if __name__ == "),
        "{}",
        edit.new_text
    );
    c.shutdown();
}

#[test]
fn hover_shows_quick_documentation() {
    let mut c = Client::start(&[("util.py", UTIL), ("main.py", MAIN)]);
    let uri = c.open("main.py");
    let hover: Option<Hover> = c.request::<request::HoverRequest>(HoverParams {
        text_document_position_params: tdp(&uri, find(MAIN, "helper('x')", 0)),
        work_done_progress_params: Default::default(),
    });
    let hover = hover.expect("hover");
    let HoverContents::Markup(m) = hover.contents else {
        panic!("markup expected")
    };
    assert_eq!(m.kind, MarkupKind::Markdown);
    assert!(
        m.value
            .starts_with("```python\ndef helper(name: str) -> str\n```"),
        "{}",
        m.value
    );
    assert!(m.value.contains("Greets someone by name."), "{}", m.value);
    assert_eq!(hover.range, Some(Range::new(pos(5, 12), pos(5, 18))));
    // Nothing on a keyword.
    let none: Option<Hover> = c.request::<request::HoverRequest>(HoverParams {
        text_document_position_params: tdp(&uri, pos(4, 1)),
        work_done_progress_params: Default::default(),
    });
    assert!(none.is_none());
    c.shutdown();
}

#[test]
fn signature_help_marks_the_active_parameter() {
    let src =
        "def area(width: int, height: int = 1) -> int:\n    return width * height\n\n\narea(3, \n";
    let mut c = Client::start(&[("m.py", src)]);
    let uri = c.open("m.py");
    let help: Option<SignatureHelp> =
        c.request::<request::SignatureHelpRequest>(SignatureHelpParams {
            context: None,
            text_document_position_params: tdp(&uri, pos(4, 8)),
            work_done_progress_params: Default::default(),
        });
    let help = help.expect("signature help");
    let sig = &help.signatures[0];
    assert_eq!(sig.label, "area(width: int, height: int = 1) -> int");
    assert_eq!(help.active_parameter, Some(1));
    let params = sig.parameters.as_ref().unwrap();
    assert_eq!(params[1].label, ParameterLabel::LabelOffsets([17, 32]));
    c.shutdown();
}

#[test]
fn definition_crosses_files_and_reaches_stdlib_stubs() {
    let mut c = Client::start(&[("util.py", UTIL), ("main.py", MAIN)]);
    let uri = c.open("main.py");
    let goto = |c: &mut Client, p: Position| -> Vec<Location> {
        let r: Option<GotoDefinitionResponse> =
            c.request::<request::GotoDefinition>(GotoDefinitionParams {
                text_document_position_params: tdp(&uri, p),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            });
        match r {
            Some(GotoDefinitionResponse::Array(v)) => v,
            Some(GotoDefinitionResponse::Scalar(l)) => vec![l],
            Some(GotoDefinitionResponse::Link(_)) => panic!("links not requested"),
            None => Vec::new(),
        }
    };
    let locs = goto(&mut c, find(MAIN, "helper('x')", 0));
    assert_eq!(locs.len(), 1);
    assert_eq!(locs[0].uri, c.uri("util.py"));
    assert_eq!(locs[0].range, Range::new(pos(0, 4), pos(0, 10)));
    // A method through an instance.
    let locs = goto(&mut c, find(MAIN, "greet(value)", 0));
    assert_eq!(locs[0].uri, c.uri("util.py"));
    assert_eq!(locs[0].range, Range::new(pos(6, 8), pos(6, 13)));
    // `os.sep` goes into the bundled stub, written out for the editor.
    let locs = goto(&mut c, find(MAIN, "sep", 0));
    let stub = path_of(&locs[0].uri);
    assert!(stub.starts_with(c.root().join(".stubs")), "{stub:?}");
    let text = std::fs::read_to_string(&stub).unwrap();
    assert!(text.contains("sep"), "stub written");
    // Requests on the opened stub keep resolving inside the stdlib.
    let stub_uri = locs[0].uri.clone();
    c.notify::<notification::DidOpenTextDocument>(DidOpenTextDocumentParams {
        text_document: TextDocumentItem {
            uri: stub_uri.clone(),
            language_id: "python".into(),
            version: 1,
            text: text.clone(),
        },
    });
    let symbols: Option<DocumentSymbolResponse> =
        c.request::<request::DocumentSymbolRequest>(DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: stub_uri },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
    assert!(matches!(symbols, Some(DocumentSymbolResponse::Nested(ref v)) if !v.is_empty()));
    // Type definition: the class of `value`.
    let r: Option<GotoDefinitionResponse> =
        c.request::<request::GotoTypeDefinition>(GotoDefinitionParams {
            text_document_position_params: tdp(&uri, find(MAIN, "value = ", 0)),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
    let Some(GotoDefinitionResponse::Array(locs)) = r else {
        panic!("type definition expected")
    };
    assert!(path_of(&locs[0].uri).ends_with("builtins.pyi"), "{locs:?}");
    c.shutdown();
}

#[test]
fn implementation_finds_subclasses() {
    let src = "class Base:\n    def run(self):\n        pass\n\n\nclass Child(Base):\n    def run(self):\n        pass\n";
    let mut c = Client::start(&[("m.py", src)]);
    let uri = c.open("m.py");
    let r: Option<GotoDefinitionResponse> =
        c.request::<request::GotoImplementation>(GotoDefinitionParams {
            text_document_position_params: tdp(&uri, pos(1, 9)),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
    let Some(GotoDefinitionResponse::Array(locs)) = r else {
        panic!("implementations expected")
    };
    assert_eq!(
        locs.iter().map(|l| l.range).collect::<Vec<_>>(),
        vec![Range::new(pos(6, 8), pos(6, 11))]
    );
    c.shutdown();
}

#[test]
fn references_and_highlights_span_the_project() {
    let mut c = Client::start(&[("util.py", UTIL), ("main.py", MAIN)]);
    let uri = c.open("main.py");
    let refs = |c: &mut Client, include: bool| -> Vec<Location> {
        let r: Option<Vec<Location>> = c.request::<request::References>(ReferenceParams {
            text_document_position: tdp(&uri, find(MAIN, "helper('x')", 0)),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: ReferenceContext {
                include_declaration: include,
            },
        });
        r.unwrap()
    };
    let all = refs(&mut c, true);
    let in_util: Vec<Range> = all
        .iter()
        .filter(|l| l.uri == c.uri("util.py"))
        .map(|l| l.range)
        .collect();
    let in_main: Vec<Range> = all
        .iter()
        .filter(|l| l.uri == uri)
        .map(|l| l.range)
        .collect();
    assert!(
        in_util.contains(&Range::new(pos(0, 4), pos(0, 10))),
        "declaration: {all:?}"
    );
    assert!(
        in_util.contains(&Range::new(pos(7, 15), pos(7, 21))),
        "use in util.py: {all:?}"
    );
    assert!(
        in_main.contains(&Range::new(pos(1, 17), pos(1, 23)))
            && in_main.contains(&Range::new(pos(5, 12), pos(5, 18))),
        "{all:?}"
    );
    let without = refs(&mut c, false);
    assert!(!without
        .iter()
        .any(|l| l.uri == c.uri("util.py") && l.range == Range::new(pos(0, 4), pos(0, 10))));
    assert_eq!(without.len(), all.len() - 1);
    let hl: Option<Vec<DocumentHighlight>> =
        c.request::<request::DocumentHighlightRequest>(DocumentHighlightParams {
            text_document_position_params: tdp(&uri, find(MAIN, "value", 1)),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
    let hl = hl.unwrap();
    assert_eq!(hl.len(), 3, "{hl:?}");
    assert_eq!(
        hl.iter()
            .filter(|h| h.kind == Some(DocumentHighlightKind::WRITE))
            .count(),
        1
    );
    c.shutdown();
}

#[test]
fn rename_edits_every_file() {
    let mut c = Client::start(&[("util.py", UTIL), ("main.py", MAIN)]);
    let uri = c.open("main.py");
    let at = find(MAIN, "helper('x')", 0);
    let prep: Option<PrepareRenameResponse> =
        c.request::<request::PrepareRenameRequest>(tdp(&uri, at));
    match prep {
        Some(PrepareRenameResponse::RangeWithPlaceholder { range, placeholder }) => {
            assert_eq!(range, Range::new(pos(5, 12), pos(5, 18)));
            assert_eq!(placeholder, "helper");
        }
        other => panic!("unexpected prepareRename result {other:?}"),
    }
    // Library names cannot be renamed.
    let err = c
        .raw_request(
            request::PrepareRenameRequest::METHOD,
            serde_json::to_value(tdp(&uri, find(MAIN, "print", 0))).unwrap(),
        )
        .unwrap_err();
    assert!(err.message.contains("library"), "{}", err.message);
    let edit: Option<WorkspaceEdit> = c.request::<request::Rename>(RenameParams {
        text_document_position: tdp(&uri, at),
        new_name: "salute".into(),
        work_done_progress_params: Default::default(),
    });
    let edit = edit.unwrap();
    let Some(DocumentChanges::Edits(docs)) = &edit.document_changes else {
        panic!("document changes expected")
    };
    let main_doc = docs.iter().find(|d| d.text_document.uri == uri).unwrap();
    assert_eq!(
        main_doc.text_document.version,
        Some(1),
        "open documents are versioned"
    );
    let util_doc = docs
        .iter()
        .find(|d| d.text_document.uri == c.uri("util.py"))
        .unwrap();
    assert_eq!(util_doc.text_document.version, None);
    let new_main = apply(MAIN, &edits_for(&edit, &uri));
    assert!(
        new_main.contains("from util import salute, Greeter")
            && new_main.contains("value = salute('x')"),
        "{new_main}"
    );
    let new_util = apply(UTIL, &edits_for(&edit, &c.uri("util.py")));
    assert!(
        new_util.starts_with("def salute(name: str)") && new_util.contains("return salute(who)"),
        "{new_util}"
    );
    // An invalid name is refused with the IDE's message.
    let err = c
        .raw_request(
            request::Rename::METHOD,
            serde_json::to_value(RenameParams {
                text_document_position: tdp(&uri, at),
                new_name: "class".into(),
                work_done_progress_params: Default::default(),
            })
            .unwrap(),
        )
        .unwrap_err();
    assert!(err.message.contains("keyword"), "{}", err.message);
    c.shutdown();
}

#[test]
fn formatting_reformats_document_and_range() {
    let src = "x=[1,2,3]\ndef f( a,b ):\n  return a+b\ny  =  {'k':1}\n";
    let mut c = Client::start(&[
        ("m.py", src),
        (".editorconfig", "root = true\n\n[*.py]\nindent_size = 4\n"),
    ]);
    let uri = c.open("m.py");
    let options = FormattingOptions {
        tab_size: 4,
        insert_spaces: true,
        ..Default::default()
    };
    let edits: Option<Vec<TextEdit>> = c.request::<request::Formatting>(DocumentFormattingParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        options: options.clone(),
        work_done_progress_params: Default::default(),
    });
    let formatted = apply(src, &edits.unwrap());
    assert_eq!(
        formatted,
        "x = [1, 2, 3]\n\n\ndef f(a, b):\n    return a + b\n\n\ny = {'k': 1}\n"
    );
    // Range formatting only touches the selected lines.
    let edits: Option<Vec<TextEdit>> =
        c.request::<request::RangeFormatting>(DocumentRangeFormattingParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            range: Range::new(pos(0, 0), pos(0, 9)),
            options,
            work_done_progress_params: Default::default(),
        });
    let partly = apply(src, &edits.unwrap());
    assert!(
        partly.starts_with("x = [1, 2, 3]\n") && partly.contains("y  =  {'k':1}"),
        "{partly}"
    );
    // `\r\n` documents keep their line breaks, for the whole file and for a range.
    let crlf = "a=1\r\nb  = 2\r\n";
    let uri = c.open_text("crlf.py", crlf);
    let edits: Option<Vec<TextEdit>> = c.request::<request::Formatting>(DocumentFormattingParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        options: FormattingOptions {
            tab_size: 4,
            insert_spaces: true,
            ..Default::default()
        },
        work_done_progress_params: Default::default(),
    });
    assert_eq!(apply(crlf, &edits.unwrap()), "a = 1\r\nb = 2\r\n");
    let edits: Option<Vec<TextEdit>> =
        c.request::<request::RangeFormatting>(DocumentRangeFormattingParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            range: Range::new(pos(1, 0), pos(1, 6)),
            options: FormattingOptions {
                tab_size: 4,
                insert_spaces: true,
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
        });
    assert_eq!(apply(crlf, &edits.unwrap()), "a=1\r\nb = 2\r\n");
    // A syntax error is reported, not formatted.
    let broken = c.open_text("broken.py", "def f(:\n");
    let err = c
        .raw_request(
            request::Formatting::METHOD,
            serde_json::to_value(DocumentFormattingParams {
                text_document: TextDocumentIdentifier { uri: broken },
                options: FormattingOptions::default(),
                work_done_progress_params: Default::default(),
            })
            .unwrap(),
        )
        .unwrap_err();
    assert!(err.message.contains("line 1"), "{}", err.message);
    c.shutdown();
}

#[test]
fn document_and_workspace_symbols() {
    let mut c = Client::start(&[("util.py", UTIL), ("main.py", MAIN)]);
    let uri = c.open("util.py");
    let r: Option<DocumentSymbolResponse> =
        c.request::<request::DocumentSymbolRequest>(DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
    let Some(DocumentSymbolResponse::Nested(symbols)) = r else {
        panic!("nested symbols expected")
    };
    let names: Vec<(&str, SymbolKind)> =
        symbols.iter().map(|s| (s.name.as_str(), s.kind)).collect();
    assert_eq!(
        names,
        vec![
            ("helper", SymbolKind::FUNCTION),
            ("Greeter", SymbolKind::CLASS)
        ]
    );
    assert_eq!(symbols[0].detail.as_deref(), Some("(name: str) -> str"));
    assert_eq!(
        symbols[0].selection_range,
        Range::new(pos(0, 4), pos(0, 10))
    );
    let greet = &symbols[1].children.as_ref().unwrap()[0];
    assert_eq!(
        (greet.name.as_str(), greet.kind),
        ("greet", SymbolKind::METHOD)
    );
    #[allow(deprecated)]
    let r: Option<WorkspaceSymbolResponse> =
        c.request::<request::WorkspaceSymbolRequest>(WorkspaceSymbolParams {
            query: "gree".into(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
    let Some(WorkspaceSymbolResponse::Flat(found)) = r else {
        panic!("symbols expected")
    };
    let names: Vec<&str> = found.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names[..2], ["greet", "Greeter"], "{names:?}");
    assert_eq!(found[0].container_name.as_deref(), Some("Greeter"));
    assert_eq!(found[0].location.uri, c.uri("util.py"));
    c.shutdown();
}

#[test]
fn folding_selection_ranges_semantic_tokens_and_inlay_hints() {
    let src = "import os\nimport sys\n\n\ndef total(values, start=0):\n    result = sum(values, start)\n    return result\n\n\nx = total([\n    1,\n    2,\n])\n";
    let mut c = Client::start(&[("m.py", src)]);
    let uri = c.open("m.py");
    let folds: Option<Vec<FoldingRange>> =
        c.request::<request::FoldingRangeRequest>(FoldingRangeParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
    let folds: Vec<(u32, u32, Option<FoldingRangeKind>)> = folds
        .unwrap()
        .into_iter()
        .map(|f| (f.start_line, f.end_line, f.kind))
        .collect();
    assert!(
        folds.contains(&(0, 1, Some(FoldingRangeKind::Imports))),
        "{folds:?}"
    );
    assert!(folds.contains(&(4, 6, None)), "function body: {folds:?}");
    assert!(
        folds.contains(&(9, 11, None)),
        "the closing bracket stays visible: {folds:?}"
    );
    let sel: Option<Vec<SelectionRange>> =
        c.request::<request::SelectionRangeRequest>(SelectionRangeParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            positions: vec![pos(5, 18)],
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
    let mut chain = Vec::new();
    let mut cur = sel.unwrap().into_iter().next();
    while let Some(s) = cur {
        chain.push(s.range);
        cur = s.parent.map(|b| *b);
    }
    assert_eq!(
        chain[0],
        Range::new(pos(5, 17), pos(5, 23)),
        "the word first"
    );
    assert!(
        chain.contains(&Range::new(pos(5, 13), pos(5, 31))),
        "then the call: {chain:?}"
    );
    let tokens: Option<SemanticTokensResult> =
        c.request::<request::SemanticTokensFullRequest>(SemanticTokensParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
    let Some(SemanticTokensResult::Tokens(tokens)) = tokens else {
        panic!("tokens expected")
    };
    // Decode to absolute positions.
    let legend = match c
        .init
        .capabilities
        .semantic_tokens_provider
        .as_ref()
        .unwrap()
    {
        SemanticTokensServerCapabilities::SemanticTokensOptions(o) => o.legend.clone(),
        SemanticTokensServerCapabilities::SemanticTokensRegistrationOptions(o) => {
            o.semantic_tokens_options.legend.clone()
        }
    };
    let (mut line, mut col) = (0, 0);
    let mut decoded = Vec::new();
    for t in &tokens.data {
        line += t.delta_line;
        col = if t.delta_line == 0 {
            col + t.delta_start
        } else {
            t.delta_start
        };
        decoded.push((
            line,
            col,
            t.length,
            legend.token_types[t.token_type as usize]
                .as_str()
                .to_string(),
        ));
    }
    assert!(
        decoded.contains(&(4, 0, 3, "keyword".into())),
        "{decoded:?}"
    );
    assert!(
        decoded.contains(&(4, 4, 5, "function".into())),
        "{decoded:?}"
    );
    assert!(
        decoded.contains(&(4, 10, 6, "parameter".into())),
        "{decoded:?}"
    );
    assert!(
        decoded.contains(&(5, 17, 6, "parameter".into())),
        "parameter use: {decoded:?}"
    );
    assert!(
        decoded.contains(&(0, 7, 2, "namespace".into())),
        "{decoded:?}"
    );
    let hints: Option<Vec<InlayHint>> = c.request::<request::InlayHintRequest>(InlayHintParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        range: Range::new(pos(0, 0), pos(13, 0)),
        work_done_progress_params: Default::default(),
    });
    let hints = hints.unwrap();
    let labels: Vec<(Position, String)> = hints
        .iter()
        .map(|h| {
            (
                h.position,
                match &h.label {
                    InlayHintLabel::String(s) => s.clone(),
                    InlayHintLabel::LabelParts(p) => p.iter().map(|p| p.value.clone()).collect(),
                },
            )
        })
        .collect();
    assert!(
        labels.contains(&(pos(9, 10), "values:".into())),
        "{labels:?}"
    );
    assert!(labels.contains(&(pos(9, 1), ": int".into())), "{labels:?}");
    c.shutdown();
}

#[test]
fn code_actions_offer_quick_fixes_intentions_and_optimize_imports() {
    let src = "import sys\nimport os\n\n\ndef f(p):\n    if p == 1:\n        return Path(p)\n    return os.sep\n";
    let mut c = Client::start(&[("m.py", src)]);
    let uri = c.open("m.py");
    let diags = c.diagnostics(&uri, Some(1));
    let unresolved: Vec<Diagnostic> = diags
        .into_iter()
        .filter(|d| d.message == "Unresolved reference 'Path'")
        .collect();
    assert_eq!(unresolved.len(), 1);
    let actions =
        |c: &mut Client, range: Range, only: Option<Vec<CodeActionKind>>| -> Vec<CodeAction> {
            let r: Option<CodeActionResponse> =
                c.request::<request::CodeActionRequest>(CodeActionParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    range,
                    context: CodeActionContext {
                        diagnostics: Vec::new(),
                        only,
                        trigger_kind: None,
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                });
            r.unwrap()
                .into_iter()
                .filter_map(|a| match a {
                    CodeActionOrCommand::CodeAction(a) => Some(a),
                    CodeActionOrCommand::Command(_) => None,
                })
                .collect()
        };
    let at_path = actions(&mut c, Range::new(pos(6, 16), pos(6, 16)), None);
    let import = at_path
        .iter()
        .find(|a| a.title == "Import 'from pathlib import Path'")
        .expect("import fix offered");
    assert_eq!(import.kind, Some(CodeActionKind::QUICKFIX));
    assert_eq!(
        import.diagnostics.as_ref().map(|d| d[0].message.as_str()),
        Some("Unresolved reference 'Path'")
    );
    let fixed = apply(src, &edits_for(import.edit.as_ref().unwrap(), &uri));
    assert!(
        fixed.starts_with("import sys\nimport os\nfrom pathlib import Path\n"),
        "{fixed}"
    );
    // Intentions on the `if`.
    let at_if = actions(
        &mut c,
        Range::new(pos(5, 5), pos(5, 5)),
        Some(vec![CodeActionKind::REFACTOR]),
    );
    assert!(at_if.iter().any(|a| a.title.contains("Invert") && a.kind == Some(CodeActionKind::REFACTOR_REWRITE)), "{:?}", at_if.iter().map(|a| &a.title).collect::<Vec<_>>());
    assert!(
        at_if
            .iter()
            .all(|a| a.kind != Some(CodeActionKind::QUICKFIX)),
        "filtered by kind"
    );
    let doc_stub = actions(&mut c, Range::new(pos(4, 5), pos(4, 5)), None);
    assert!(doc_stub
        .iter()
        .any(|a| a.title == "Insert documentation string stub"));
    // Optimize imports removes the unused `sys`.
    let organize = actions(
        &mut c,
        Range::new(pos(0, 0), pos(0, 0)),
        Some(vec![CodeActionKind::SOURCE_ORGANIZE_IMPORTS]),
    );
    assert_eq!(organize.len(), 1);
    let optimized = apply(src, &edits_for(organize[0].edit.as_ref().unwrap(), &uri));
    assert!(
        !optimized.contains("import sys") && optimized.contains("import os"),
        "{optimized}"
    );
    c.shutdown();
}

#[test]
fn import_fixes_offer_project_symbols() {
    let src = "def run():\n    return Greeter()\n";
    let mut c = Client::start(&[("util.py", UTIL), ("app.py", src)]);
    let uri = c.open("app.py");
    let diags = c.diagnostics(&uri, Some(1));
    assert!(
        diags
            .iter()
            .any(|d| d.message == "Unresolved reference 'Greeter'"),
        "{diags:?}"
    );
    // The project's symbols are indexed in the background.
    let deadline = Instant::now() + TIMEOUT;
    let action = loop {
        let r: Option<CodeActionResponse> =
            c.request::<request::CodeActionRequest>(CodeActionParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                range: Range::new(pos(1, 13), pos(1, 13)),
                context: CodeActionContext {
                    diagnostics: Vec::new(),
                    only: Some(vec![CodeActionKind::QUICKFIX]),
                    trigger_kind: None,
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            });
        let found = r.unwrap().into_iter().find_map(|a| match a {
            CodeActionOrCommand::CodeAction(a)
                if a.title == "Import 'from util import Greeter'" =>
            {
                Some(a)
            }
            _ => None,
        });
        if let Some(a) = found {
            break a;
        }
        assert!(Instant::now() < deadline, "project import fix offered");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(action.is_preferred, Some(true));
    let fixed = apply(src, &edits_for(action.edit.as_ref().unwrap(), &uri));
    assert_eq!(
        fixed,
        "from util import Greeter\ndef run():\n    return Greeter()\n"
    );
    c.shutdown();
}

#[test]
fn call_hierarchy_follows_callers_and_callees() {
    let mut c = Client::start(&[("util.py", UTIL), ("main.py", MAIN)]);
    let uri = c.open("util.py");
    let items: Option<Vec<CallHierarchyItem>> =
        c.request::<request::CallHierarchyPrepare>(CallHierarchyPrepareParams {
            text_document_position_params: tdp(&uri, pos(0, 5)),
            work_done_progress_params: Default::default(),
        });
    let item = items.unwrap().remove(0);
    assert_eq!(
        (item.name.as_str(), item.kind),
        ("helper", SymbolKind::FUNCTION)
    );
    let incoming: Option<Vec<CallHierarchyIncomingCall>> = c
        .request::<request::CallHierarchyIncomingCalls>(CallHierarchyIncomingCallsParams {
            item: item.clone(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
    let incoming = incoming.unwrap();
    for call in &incoming {
        let source = if call.from.name == "greet" {
            UTIL
        } else {
            MAIN
        };
        assert!(!call.from_ranges.is_empty());
        for range in &call.from_ranges {
            assert_eq!(range.start.line, range.end.line);
            let line = source.lines().nth(range.start.line as usize).unwrap();
            assert_eq!(
                &line[range.start.character as usize..range.end.character as usize],
                "helper",
                "incoming ranges must identify calls, not caller declarations"
            );
        }
    }
    let mut callers: Vec<String> = incoming.into_iter().map(|c| c.from.name).collect();
    callers.sort();
    assert_eq!(callers, vec!["greet", "main"]);
    let main_uri = c.open("main.py");
    let items: Option<Vec<CallHierarchyItem>> =
        c.request::<request::CallHierarchyPrepare>(CallHierarchyPrepareParams {
            text_document_position_params: tdp(&main_uri, pos(4, 5)),
            work_done_progress_params: Default::default(),
        });
    let outgoing: Option<Vec<CallHierarchyOutgoingCall>> = c
        .request::<request::CallHierarchyOutgoingCalls>(CallHierarchyOutgoingCallsParams {
            item: items.unwrap().remove(0),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
    let callees: Vec<String> = outgoing.unwrap().into_iter().map(|c| c.to.name).collect();
    assert!(
        callees.contains(&"helper".to_string()) && callees.contains(&"greet".to_string()),
        "{callees:?}"
    );
    c.shutdown();
}

#[test]
fn incoming_calls_distinguish_module_calls_from_method_declarations() {
    let source =
        "def target(): pass\ntarget()\nclass Owner:\n    def method(self):\n        target()\n";
    let mut c = Client::start(&[("calls.py", source)]);
    let uri = c.open("calls.py");
    let items: Option<Vec<CallHierarchyItem>> =
        c.request::<request::CallHierarchyPrepare>(CallHierarchyPrepareParams {
            text_document_position_params: tdp(&uri, find(source, "target", 0)),
            work_done_progress_params: Default::default(),
        });
    let incoming: Option<Vec<CallHierarchyIncomingCall>> = c
        .request::<request::CallHierarchyIncomingCalls>(CallHierarchyIncomingCallsParams {
            item: items.unwrap().remove(0),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
    let incoming = incoming.unwrap();
    assert_eq!(incoming.len(), 2);
    for (name, line) in [("calls", 1), ("method", 4)] {
        let call = incoming.iter().find(|call| call.from.name == name).unwrap();
        assert_eq!(call.from_ranges.len(), 1);
        assert_eq!(call.from_ranges[0].start.line, line);
    }
    c.shutdown();
}

#[test]
fn cancellation_answers_once_and_the_server_keeps_working() {
    let mut c = Client::start(&[("util.py", UTIL), ("main.py", MAIN)]);
    let uri = c.open("main.py");
    let params = serde_json::to_value(ReferenceParams {
        text_document_position: tdp(&uri, find(MAIN, "helper('x')", 0)),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
        context: ReferenceContext {
            include_declaration: true,
        },
    })
    .unwrap();
    let id = c.send_request(request::References::METHOD, params);
    let numeric = match serde_json::to_value(&id).unwrap() {
        Value::Number(n) => n.as_i64().unwrap() as i32,
        other => panic!("{other:?}"),
    };
    c.notify::<notification::Cancel>(CancelParams {
        id: NumberOrString::Number(numeric),
    });
    let resp = c.response(&id);
    if let Err(e) = &resp.response_result {
        assert_eq!(e.code, lsp_server::ErrorCode::RequestCanceled as i32);
    }
    // Unknown requests are answered with MethodNotFound; normal ones keep working.
    let err = c
        .raw_request("textDocument/unknownThing", json!({}))
        .unwrap_err();
    assert_eq!(err.code, lsp_server::ErrorCode::MethodNotFound as i32);
    let hover: Option<Hover> = c.request::<request::HoverRequest>(HoverParams {
        text_document_position_params: tdp(&uri, find(MAIN, "helper('x')", 0)),
        work_done_progress_params: Default::default(),
    });
    assert!(hover.is_some());
    // No second answer to the cancelled request arrives.
    std::thread::sleep(Duration::from_millis(200));
    while let Ok(m) = c.conn.receiver.try_recv() {
        assert!(
            !matches!(m, Message::Response(_)),
            "unexpected extra response {m:?}"
        );
    }
    c.shutdown();
}

#[test]
fn closing_a_document_clears_its_diagnostics_and_exit_without_shutdown_fails() {
    let mut c = Client::start(&[("m.py", "print(nope)\n")]);
    let uri = c.open("m.py");
    assert!(!c.diagnostics(&uri, Some(1)).is_empty());
    c.notify::<notification::DidCloseTextDocument>(DidCloseTextDocumentParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
    });
    assert!(c.diagnostics(&uri, None).is_empty());
    // `exit` before `shutdown` ends the server with an error (exit code 1).
    c.notify::<notification::Exit>(());
    let result = c.server.take().unwrap().join().unwrap();
    assert!(result.is_err());
}

fn codes(diags: &[Diagnostic]) -> Vec<String> {
    diags
        .iter()
        .filter_map(|d| match &d.code {
            Some(NumberOrString::String(s)) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn watched_file_events_refresh_imports() {
    let mut c = Client::start(&[("main.py", "import newmod\n\nprint(newmod.VALUE)\n")]);
    let uri = c.open("main.py");
    let diags = c.diagnostics(&uri, Some(1));
    assert!(
        diags
            .iter()
            .any(|d| d.message == "No module named 'newmod'"),
        "{diags:?}"
    );
    // The module appears on disk and the client reports it.
    std::fs::write(c.root().join("newmod.py"), "VALUE = 1\n").unwrap();
    c.notify::<notification::DidChangeWatchedFiles>(DidChangeWatchedFilesParams {
        changes: vec![FileEvent {
            uri: c.uri("newmod.py"),
            typ: FileChangeType::CREATED,
        }],
    });
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let diags = c.diagnostics(&uri, Some(1));
        if !diags.iter().any(|d| d.message.contains("newmod")) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the new module resolves: {diags:?}"
        );
    }
    c.shutdown();
}

#[test]
fn the_server_watches_files_when_the_client_cannot() {
    // The test client does not offer dynamic watched-file registration.
    let mut c = Client::start(&[("main.py", "import later_mod\n\nprint(later_mod.VALUE)\n")]);
    let uri = c.open("main.py");
    let diags = c.diagnostics(&uri, Some(1));
    assert!(
        diags
            .iter()
            .any(|d| d.message == "No module named 'later_mod'"),
        "{diags:?}"
    );
    std::fs::write(c.root().join("later_mod.py"), "VALUE = 1\n").unwrap();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let diags = c.diagnostics(&uri, Some(1));
        if !diags.iter().any(|d| d.message.contains("later_mod")) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the new module is noticed: {diags:?}"
        );
    }
    c.shutdown();
}

#[test]
fn configuration_changes_switch_inspections() {
    let mut c = Client::start(&[("m.py", "import os\n\n\ndef f():\n    pass\n")]);
    let uri = c.open("m.py");
    assert!(codes(&c.diagnostics(&uri, Some(1))).contains(&"PyUnusedImports".to_string()));
    c.notify::<notification::DidChangeConfiguration>(DidChangeConfigurationParams { settings: json!({ "rustpy": { "settings": { "inspections": { "unused_imports": false } } } }) });
    // Diagnostics published before the change was handled may still arrive first.
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let diags = c.diagnostics(&uri, Some(1));
        if !codes(&diags).contains(&"PyUnusedImports".to_string()) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the inspection is switched off: {diags:?}"
        );
    }
    c.shutdown();
}

#[test]
fn workspace_folders_can_be_added_and_removed() {
    let mut c = Client::start(&[
        (
            "app/main.py",
            "import shared_lib\n\nprint(shared_lib.NAME)\n",
        ),
        ("lib/shared_lib.py", "NAME = 'x'\n"),
    ]);
    // Narrow the workspace from the whole temporary directory to `app`.
    let root = c.root();
    let folder = |p: &Path| WorkspaceFolder {
        uri: uri(p),
        name: p.file_name().unwrap().to_string_lossy().into_owned(),
    };
    c.notify::<notification::DidChangeWorkspaceFolders>(DidChangeWorkspaceFoldersParams {
        event: WorkspaceFoldersChangeEvent {
            added: vec![folder(&root.join("app"))],
            removed: vec![folder(&root)],
        },
    });
    let uri_main = c.open("app/main.py");
    let diags = c.diagnostics(&uri_main, Some(1));
    assert!(
        diags
            .iter()
            .any(|d| d.message == "No module named 'shared_lib'"),
        "{diags:?}"
    );
    // A second root makes its modules importable.
    c.notify::<notification::DidChangeWorkspaceFolders>(DidChangeWorkspaceFoldersParams {
        event: WorkspaceFoldersChangeEvent {
            added: vec![folder(&root.join("lib"))],
            removed: Vec::new(),
        },
    });
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let diags = c.diagnostics(&uri_main, Some(1));
        if !diags.iter().any(|d| d.message.contains("shared_lib")) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the import resolves after the folder is added: {diags:?}"
        );
    }
    c.shutdown();
}

#[test]
fn unsaved_names_refresh_auto_imports_and_quick_fixes_without_workspace_symbol_requests() {
    let mut c = Client::start(&[
        ("helper.py", "class OldChoice: pass\n"),
        ("main.py", "NewChoice()\n"),
    ]);
    let main = c.open("main.py");
    let _ = c.diagnostics(&main, Some(1));
    let helper = c.open_text("helper.py", "class NewChoice: pass\n");
    let complete = |c: &mut Client, prefix: &str| {
        let r: Option<CompletionResponse> = c.request::<request::Completion>(CompletionParams {
            text_document_position: tdp(&main, pos(0, prefix.len() as u32)),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        });
        let Some(CompletionResponse::List(list)) = r else {
            panic!("completion list")
        };
        list.items
    };
    let items = complete(&mut c, "NewChoice");
    let item = items
        .iter()
        .find(|i| i.label == "NewChoice")
        .expect("unsaved renamed class is importable immediately");
    assert_eq!(
        item.additional_text_edits.as_ref().unwrap()[0].new_text,
        "from helper import NewChoice\n"
    );
    assert!(!items.iter().any(|i| i.label == "OldChoice"));
    let actions: Option<CodeActionResponse> =
        c.request::<request::CodeActionRequest>(CodeActionParams {
            text_document: TextDocumentIdentifier { uri: main.clone() },
            range: Range::new(pos(0, 0), pos(0, 9)),
            context: CodeActionContext {
                diagnostics: Vec::new(),
                only: Some(vec![CodeActionKind::QUICKFIX]),
                trigger_kind: None,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
    assert!(actions.unwrap().iter().any(|a| matches!(a, CodeActionOrCommand::CodeAction(a) if a.title.contains("NewChoice") && a.title.to_lowercase().contains("import"))), "an import fix uses the unsaved definition");
    c.notify::<notification::DidChangeTextDocument>(DidChangeTextDocumentParams {
        text_document: VersionedTextDocumentIdentifier {
            uri: helper.clone(),
            version: 2,
        },
        content_changes: vec![TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: "class FreshChoice: pass\n".into(),
        }],
    });
    c.notify::<notification::DidChangeTextDocument>(DidChangeTextDocumentParams {
        text_document: VersionedTextDocumentIdentifier {
            uri: main.clone(),
            version: 2,
        },
        content_changes: vec![TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: "FreshChoice()\n".into(),
        }],
    });
    let error = c
        .raw_request(
            request::ResolveCompletionItem::METHOD,
            serde_json::to_value(item).unwrap(),
        )
        .unwrap_err();
    assert_eq!(
        error.code,
        lsp_server::ErrorCode::ContentModified as i32,
        "stale completion edits must not be returned again"
    );
    assert!(complete(&mut c, "FreshChoice")
        .iter()
        .any(|i| i.label == "FreshChoice"));
    c.notify::<notification::DidCloseTextDocument>(DidCloseTextDocumentParams {
        text_document: TextDocumentIdentifier { uri: helper },
    });
    c.open_text("brand_new.py", "class FreshChoice: pass\n");
    let items = complete(&mut c, "FreshChoice");
    let item = items
        .iter()
        .find(|i| i.label == "FreshChoice")
        .expect("new unsaved file is importable");
    assert_eq!(
        item.additional_text_edits.as_ref().unwrap()[0].new_text,
        "from brand_new import FreshChoice\n"
    );
    c.shutdown();
}

#[test]
fn invalid_initial_shared_project_reports_error_and_keeps_open_documents_usable() {
    let mut c = Client::start(&[
        (".rustpy/project.json", "<<<<<<< conflict"),
        ("main.py", "def local():\n    return 1\n"),
    ]);
    let document = c.open("main.py");
    let symbols: Option<DocumentSymbolResponse> =
        c.request::<request::DocumentSymbolRequest>(DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: document },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
    assert!(symbols.is_some());
    assert!(
        c.notifications
            .iter()
            .filter(|n| n.method == notification::LogMessage::METHOD)
            .map(|n| serde_json::from_value::<LogMessageParams>(n.params.clone()).unwrap())
            .any(|p| p.typ == MessageType::ERROR
                && p.message.contains("project.json")
                && p.message.contains("not valid")),
        "{:?}",
        c.notifications
    );
    let symbols: Option<WorkspaceSymbolResponse> =
        c.request::<request::WorkspaceSymbolRequest>(WorkspaceSymbolParams {
            query: "local".into(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
    // Open documents remain available while an invalid initial scope contributes
    // no other files from disk.
    assert!(symbols.is_some());
    c.shutdown();
}

#[test]
fn saved_shared_project_roots_reach_protocol_consumers_and_failed_reload_keeps_scope() {
    let src = "from helper import answer\nfrom test_support import stub\nfrom external import attached_scope_member\nanswer()\nstub()\nattached_scope_member()\n";
    let mut c = Client::start(&[
        ("main.py", src),
        ("src/helper.py", "def answer():\n    return 1\n"),
        ("tests/test_support.pyi", "def stub() -> int: ...\n"),
        ("vendor/hidden.py", "def hidden_scope_member(): pass\n"),
        ("ignored.py", "def ignored_scope_member(): pass\n"),
        (".git/config", ""),
        (".gitignore", "ignored.py\n"),
    ]);
    let shared = tempfile::tempdir().unwrap();
    std::fs::write(
        shared.path().join("external.py"),
        "def attached_scope_member():\n    return 1\n",
    )
    .unwrap();
    let root = c.root();
    rustpy_project::ProjectModel {
        content_roots: vec![shared.path().into()],
        source_roots: vec![root.join("src")],
        test_roots: vec![root.join("tests")],
        excluded: vec![root.join("vendor")],
        ..Default::default()
    }
    .save(&root)
    .unwrap();
    c.notify::<notification::DidChangeWatchedFiles>(DidChangeWatchedFilesParams {
        changes: vec![FileEvent {
            uri: c.uri(".rustpy/project.json"),
            typ: FileChangeType::CREATED,
        }],
    });
    let document = c.open("main.py");
    let definition = |c: &mut Client, name: &str| -> Vec<Location> {
        let result: Option<GotoDefinitionResponse> =
            c.request::<request::GotoDefinition>(GotoDefinitionParams {
                text_document_position_params: tdp(&document, find(src, &format!("{name}()"), 0)),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            });
        match result {
            Some(GotoDefinitionResponse::Array(v)) => v,
            Some(GotoDefinitionResponse::Scalar(l)) => vec![l],
            _ => Vec::new(),
        }
    };
    assert_eq!(definition(&mut c, "answer")[0].uri, c.uri("src/helper.py"));
    assert_eq!(
        definition(&mut c, "stub")[0].uri,
        c.uri("tests/test_support.pyi")
    );
    assert_eq!(
        definition(&mut c, "attached_scope_member")[0].uri,
        uri(&shared.path().join("external.py"))
    );
    for query in ["hidden_scope_member", "ignored_scope_member"] {
        let symbols: Option<WorkspaceSymbolResponse> = c
            .request::<request::WorkspaceSymbolRequest>(WorkspaceSymbolParams {
                query: query.into(),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            });
        assert!(
            match symbols {
                None => true,
                Some(WorkspaceSymbolResponse::Flat(v)) => v.is_empty(),
                Some(WorkspaceSymbolResponse::Nested(v)) => v.is_empty(),
            },
            "{query} escaped the project scope"
        );
    }
    std::fs::write(
        rustpy_project::ProjectModel::path_for(&root),
        "<<<<<<< conflict",
    )
    .unwrap();
    c.notify::<notification::DidChangeWatchedFiles>(DidChangeWatchedFilesParams {
        changes: vec![FileEvent {
            uri: c.uri(".rustpy/project.json"),
            typ: FileChangeType::CHANGED,
        }],
    });
    assert_eq!(definition(&mut c, "answer")[0].uri, c.uri("src/helper.py"));
    assert!(
        c.notifications
            .iter()
            .filter(|n| n.method == notification::LogMessage::METHOD)
            .map(|n| serde_json::from_value::<LogMessageParams>(n.params.clone()).unwrap())
            .any(|p| p.typ == MessageType::ERROR && p.message.contains("project.json")),
        "{:?}",
        c.notifications
    );
    c.shutdown();
}

#[test]
fn attached_root_changes_invalidate_lsp_cache_with_native_and_dynamic_clients() {
    for dynamic in [false, true] {
        let mut capabilities = serde_json::to_value(client_caps()).unwrap();
        capabilities["workspace"]["didChangeWatchedFiles"] =
            json!({"dynamicRegistration": dynamic});
        let mut c = Client::start_with(
            &[("main.py", "x = 1\n")],
            serde_json::from_value(capabilities).unwrap(),
            json!({}),
        );
        let attached = tempfile::tempdir().unwrap();
        std::fs::write(
            attached.path().join("existing.py"),
            "def existing_attached_scope_file(): pass\n",
        )
        .unwrap();
        let root = c.root();
        rustpy_project::ProjectModel {
            content_roots: vec![attached.path().into()],
            ..Default::default()
        }
        .save(&root)
        .unwrap();
        c.notify::<notification::DidChangeWatchedFiles>(DidChangeWatchedFilesParams {
            changes: vec![FileEvent {
                uri: c.uri(".rustpy/project.json"),
                typ: FileChangeType::CREATED,
            }],
        });
        let query = |c: &mut Client, name: &str| -> bool {
            let result = c.raw_request(
                request::WorkspaceSymbolRequest::METHOD,
                serde_json::to_value(WorkspaceSymbolParams {
                    query: name.into(),
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                })
                .unwrap(),
            );
            let symbols: Option<WorkspaceSymbolResponse> = match result {
                Ok(value) => serde_json::from_value(value).unwrap(),
                // The watcher change this test is awaiting can retire a query
                // already in flight. Retry only that protocol response within
                // the same deadline; all other failures remain test failures.
                Err(error) if error.code == lsp_server::ErrorCode::ContentModified as i32 => {
                    return false;
                }
                Err(error) => panic!("workspace/symbol failed: {}", error.message),
            };
            match symbols {
                Some(WorkspaceSymbolResponse::Flat(v)) => v.iter().any(|s| s.name == name),
                Some(WorkspaceSymbolResponse::Nested(v)) => v.iter().any(|s| s.name == name),
                None => false,
            }
        };
        let initial_deadline = Instant::now() + Duration::from_secs(5);
        while !query(&mut c, "existing_attached_scope_file") {
            assert!(
                Instant::now() < initial_deadline,
                "attached root was not indexed"
            );
            std::thread::sleep(Duration::from_millis(30));
        }
        std::fs::write(
            attached.path().join("new.py"),
            "def new_attached_scope_file(): pass\n",
        )
        .unwrap();
        // No client notification: this must arrive through the attached-root
        // watcher, before the preserved ten-second enumeration cache expires.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut found = false;
        while Instant::now() < deadline {
            if query(&mut c, "new_attached_scope_file") {
                found = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        assert!(
            found,
            "attached root was not watched (dynamic client: {dynamic})"
        );
        c.shutdown();
    }
}

#[test]
fn shared_positions_drive_unicode_incremental_diagnostics_completion_and_tokens() {
    // Hand-counted prefix `"é😀é"; `: UTF-8 bytes13, UTF-16 units9, scalars8.
    // The source deliberately mixes CRLF, raw CR and LF.
    for (kind, column) in [
        (PositionEncodingKind::UTF8, 13),
        (PositionEncodingKind::UTF16, 9),
        (PositionEncodingKind::UTF32, 8),
    ] {
        let mut caps = client_caps();
        caps.general.as_mut().unwrap().position_encodings = Some(vec![kind.clone()]);
        let mut c = Client::start_with(
            &[("m.py", "known = 1\r\n\"é😀é\"; missing\rprint(known)\n")],
            caps,
            json!({}),
        );
        assert_eq!(c.init.capabilities.position_encoding, Some(kind));
        let uri = c.open("m.py");
        let diagnostics = c.diagnostics(&uri, Some(1));
        let missing = diagnostics
            .iter()
            .find(|d| d.message == "Unresolved reference 'missing'")
            .expect("missing diagnostic");
        assert_eq!(
            missing.range,
            Range::new(pos(1, column), pos(1, column + 7))
        );
        c.notify::<notification::DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri.clone(),
                version: 2,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(Range::new(pos(1, column), pos(1, column + 7))),
                range_length: None,
                text: "kn".into(),
            }],
        });
        let diagnostics = c.diagnostics(&uri, Some(2));
        let partial = diagnostics
            .iter()
            .find(|d| d.message == "Unresolved reference 'kn'")
            .expect("incremental replacement diagnostic");
        assert_eq!(
            partial.range,
            Range::new(pos(1, column), pos(1, column + 2))
        );
        let completion: Option<CompletionResponse> =
            c.request::<request::Completion>(CompletionParams {
                text_document_position: tdp(&uri, pos(1, column + 2)),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: None,
            });
        let Some(CompletionResponse::List(list)) = completion else {
            panic!("completion list")
        };
        let known = list
            .items
            .iter()
            .find(|i| i.label == "known")
            .expect("known variable completion");
        let Some(CompletionTextEdit::Edit(edit)) = &known.text_edit else {
            panic!("completion edit: {:?}", known.text_edit)
        };
        assert_eq!(edit.range, Range::new(pos(1, column), pos(1, column + 2)));
        c.notify::<notification::DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri.clone(),
                version: 3,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: Some(Range::new(pos(1, column), pos(1, column + 2))),
                range_length: None,
                text: "known".into(),
            }],
        });
        assert!(!c
            .diagnostics(&uri, Some(3))
            .iter()
            .any(|d| d.message.starts_with("Unresolved reference")));
        let tokens: Option<SemanticTokensResult> =
            c.request::<request::SemanticTokensFullRequest>(SemanticTokensParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            });
        let Some(SemanticTokensResult::Tokens(tokens)) = tokens else {
            panic!("semantic tokens")
        };
        let (mut line, mut character) = (0, 0);
        let mut decoded = Vec::new();
        for token in tokens.data {
            line += token.delta_line;
            character = if token.delta_line == 0 {
                character + token.delta_start
            } else {
                token.delta_start
            };
            decoded.push((line, character, token.length));
        }
        assert!(
            decoded.contains(&(1, 0, column - 2)),
            "Unicode string token: {decoded:?}"
        );
        assert!(
            decoded.contains(&(1, column, 5)),
            "edited name token: {decoded:?}"
        );
        assert!(
            decoded.contains(&(2, 6, 5)),
            "raw-CR following line token: {decoded:?}"
        );
        c.shutdown();
    }
}

#[path = "basic_completion_tests.rs"]
mod basic_completion_tests;
