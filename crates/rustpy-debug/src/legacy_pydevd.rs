//! Command translation for the legacy pydevd quoted-line protocol. Wire schemas
//! follow pydevd's pydevd_comm.py / pydevd_process_net_command.py, not debugpy's
//! DAP mode.
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
#[derive(Clone)]
struct Reference {
    thread: String,
    frame: String,
    scope: String,
    attrs: String,
}
struct Pending {
    request: Value,
    kind: Reply,
    deadline: Instant,
}
enum Reply {
    Threads,
    Stack {
        thread: String,
    },
    Variables {
        reference: Reference,
        start: usize,
        count: usize,
    },
    Evaluate {
        reference: Reference,
        temp: String,
    },
    SetValue {
        reference: Reference,
        name: String,
    },
    Completions,
    Source,
    Exception,
}
#[derive(Default)]
pub(crate) struct State {
    pub dap: Vec<Value>,
    pub wire: Vec<String>,
    pub finished: bool,
    seq: u64,
    wire_seq: u64,
    next_id: i64,
    connected: bool,
    attach: Option<Value>,
    just_my_code: bool,
    threads: HashMap<String, i64>,
    frames: HashMap<i64, (String, String)>,
    stacks: HashMap<String, Vec<Value>>,
    refs: HashMap<i64, Reference>,
    pending: HashMap<u64, Pending>,
    breakpoints: HashMap<String, (String, Vec<i64>)>,
    django: bool,
    jinja: bool,
}
impl State {
    fn id(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
    }
    fn event(&mut self, name: &str, body: Value) {
        self.seq += 1;
        self.push_dap(json!({"seq":self.seq,"type":"event","event":name,"body":body}));
    }
    fn push_dap(&mut self, value: Value) {
        let used = self
            .dap
            .iter()
            .map(|value| value.to_string().len())
            .sum::<usize>();
        if used + value.to_string().len() > 16 * 1024 * 1024 {
            self.finished = true;
            return;
        }
        self.dap.push(value);
    }
    fn response(&mut self, request: &Value, body: Value, error: Option<&str>) {
        self.seq += 1;
        let mut value = json!({"seq":self.seq,"type":"response","request_seq":request["seq"],"command":request["command"],"success":error.is_none(),"body":body});
        if let Some(error) = error {
            value["message"] = error.into();
        }
        self.push_dap(value);
    }
    fn send(&mut self, command: u32, text: &str) -> u64 {
        self.wire_seq += 2;
        let seq = self.wire_seq + 1;
        let line = format!("{command}\t{seq}\t{text}\n");
        if line.len() > 8 * 1024 * 1024
            || self.wire.iter().map(String::len).sum::<usize>() + line.len() > 16 * 1024 * 1024
        {
            self.finished = true;
            self.event(
                "output",
                json!({"category":"stderr","output":"Legacy pydevd command budget exceeded\n"}),
            );
            return seq;
        }
        self.wire.push(line);
        seq
    }
    fn pending(&mut self, request: Value, command: u32, text: &str, kind: Reply) {
        if self.pending.len() >= 4096 {
            self.response(
                &request,
                json!({}),
                Some("Too many pending legacy requests"),
            );
            return;
        }
        let seq = self.send(command, text);
        self.pending.insert(
            seq,
            Pending {
                request,
                kind,
                deadline: Instant::now() + Duration::from_secs(25),
            },
        );
    }
    fn thread(&mut self, wire: &str) -> i64 {
        if let Some(id) = self.threads.get(wire) {
            *id
        } else {
            let id = self.id();
            self.threads.insert(wire.into(), id);
            id
        }
    }
    fn wire_thread(&self, id: i64) -> Option<String> {
        self.threads
            .iter()
            .find(|(_, value)| **value == id)
            .map(|(name, _)| name.clone())
    }
    fn reference(&mut self, reference: Reference) -> i64 {
        if self.refs.len() >= 100000 {
            return 0;
        }
        let id = self.id();
        self.refs.insert(id, reference);
        id
    }
    fn frame(&self, args: &Value) -> Option<Reference> {
        let id = args["frameId"]
            .as_i64()
            .or_else(|| self.frames.keys().max().copied())?;
        let (thread, frame) = self.frames.get(&id)?;
        Some(Reference {
            thread: thread.clone(),
            frame: frame.clone(),
            scope: "FRAME".into(),
            attrs: String::new(),
        })
    }
    pub fn connected(&mut self) {
        self.connected = true;
        self.send(
            501,
            if cfg!(windows) {
                "1.0\tWINDOWS\tID"
            } else {
                "1.0\tUNIX\tID"
            },
        );
        if self.attach.is_some() {
            self.event("initialized", json!({}));
        }
    }
    pub fn request(&mut self, request: Value) {
        if request["type"] != "request" {
            return;
        }
        let args = request["arguments"].clone();
        let command = request["command"].as_str().unwrap_or("").to_owned();
        match command.as_str(){
   "initialize"=>self.response(&request,json!({"supportsConfigurationDoneRequest":true,"supportsSetVariable":true,"supportsCompletionsRequest":true,"supportsExceptionInfoRequest":true,"supportsVariablePaging":true,"exceptionBreakpointFilters":[{"filter":"raised","label":"On raise"},{"filter":"uncaught","label":"On termination"}]}),None),
   "attach"=>{self.django=args["django"].as_bool().unwrap_or(false);self.jinja=args["jinja"].as_bool().unwrap_or(false);self.just_my_code=args["justMyCode"].as_bool().unwrap_or(true);self.attach=Some(request);self.send(146,&format!("\t{}",if args["showReturnValue"].as_bool().unwrap_or(false){1}else{0}));if self.connected{self.event("initialized",json!({}));}},
   "configurationDone"=>{self.send(101,"");self.response(&request,json!({}),None);if let Some(attach)=self.attach.take(){self.response(&attach,json!({}),None);}},
   "threads"=>self.pending(request,102,"",Reply::Threads),
   "stackTrace"=>{let Some(thread)=args["threadId"].as_i64().and_then(|id|self.wire_thread(id))else{self.response(&request,json!({}),Some("Unknown thread"));return;};self.pending(request,152,&thread,Reply::Stack{thread:thread.clone()});},
   "scopes"=>{let Some(reference)=self.frame(&args)else{self.response(&request,json!({}),Some("No suspended frame"));return;};let local=self.reference(reference.clone());let global=self.reference(Reference{scope:"GLOBAL".into(),..reference});self.response(&request,json!({"scopes":[{"name":"Locals","variablesReference":local,"expensive":false},{"name":"Globals","variablesReference":global,"expensive":true}]}),None);},
   "variables"=>{let Some(reference)=args["variablesReference"].as_i64().and_then(|id|self.refs.get(&id)).cloned()else{self.response(&request,json!({}),Some("Expired variable reference"));return;};let start=args["start"].as_u64().unwrap_or(0)as usize;let count=args["count"].as_u64().unwrap_or(100).clamp(1,1000)as usize;let(command,text)=if reference.attrs.is_empty(){(114,format!("{}\t{}\t{}\t0",reference.thread,reference.frame,reference.scope))}else{(110,format!("{}\t{}\t{}\t{}",reference.thread,reference.frame,reference.scope,reference.attrs))};self.pending(request,command,&text,Reply::Variables{reference,start,count});},
   "evaluate"=>{let Some(reference)=self.frame(&args)else{self.response(&request,json!({}),Some("No suspended frame"));return;};let expression=args["expression"].as_str().unwrap_or("");let mode=if args["context"]=="repl"{"exec"}else{"eval"};
    // Expressions are carried as one-line Python literals, preserving tabs and
    // newlines without injecting quoted-line protocol delimiters.
    let source=serde_json::to_string(expression).unwrap();
    let expression=if mode=="exec"{format!("eval(compile({source}, '<RustPy debugger>', 'eval' if len(__import__('ast').parse({source}).body)==1 and isinstance(__import__('ast').parse({source}).body[0], __import__('ast').Expr) else 'exec'), globals(), locals())")}else{format!("eval(compile({source}, '<RustPy debugger>', 'eval'), globals(), locals())")};

    let temp=format!("_rustpy_legacy_value_{}",self.wire_seq+2);let text=format!("{}\t{}\tLOCAL\t{}\t0\t{}",reference.thread,reference.frame,expression,temp);self.pending(request,113,&text,Reply::Evaluate{reference,temp});},
   "setVariable"=>{let Some(reference)=args["variablesReference"].as_i64().and_then(|id|self.refs.get(&id)).cloned()else{self.response(&request,json!({}),Some("Expired variable reference"));return;};let name=args["name"].as_str().unwrap_or("").to_owned();if !field(&name){self.response(&request,json!({}),Some("Variable name contains protocol delimiters"));return;}let attrs=if reference.attrs.is_empty(){name.clone()}else{format!("{}\t{name}",reference.attrs)};let value=format!("eval(compile({}, '<RustPy value>', 'eval'), globals(), locals())",serde_json::to_string(args["value"].as_str().unwrap_or("")).unwrap());let text=format!("{}\t{}\t{}\t{attrs}\t{value}",reference.thread,reference.frame,reference.scope);self.pending(request,117,&text,Reply::SetValue{reference,name});},
   "completions"=>{let Some(reference)=self.frame(&args)else{self.response(&request,json!({"targets":[]}),None);return;};let text=args["text"].as_str().unwrap_or("");if !field(text){self.response(&request,json!({"targets":[]}),Some("Complete one line at a time"));return;}let text=format!("{}\t{}\tLOCAL\t{text}",reference.thread,reference.frame);self.pending(request,120,&text,Reply::Completions);},
   "setBreakpoints"=>self.set_breakpoints(&request,&args),
   "setExceptionBreakpoints"=>{let filters=args["filters"].as_array().cloned().unwrap_or_default();let mut raised=filters.iter().any(|value|value=="raised");let mut uncaught=filters.iter().any(|value|value=="uncaught"||value=="userUnhandled");let mut names=Vec::new();if let Some(options)=args["exceptionOptions"].as_array(){for option in options{raised|=option["breakMode"]=="always";uncaught|=option["breakMode"]=="unhandled"||option["breakMode"]=="userUnhandled";if let Some(path)=option["path"].as_array(){if let Some(last)=path.last(){if let Some(values)=last["names"].as_array(){for value in values{if let Some(name)=value.as_str().filter(|name|field(name)&&!name.contains(';')){names.push(name.to_owned());}}}}}}}
    if names.is_empty(){names.push("BaseException".into());}self.send(131,&format!("{uncaught};{raised};true;false;{};{}",self.just_my_code,names.join(";")));self.response(&request,json!({}),None);},
   "continue"|"next"|"stepIn"|"stepOut"|"pause"|"rustpyForceStepInto"=>{let thread=if command=="continue"||command=="pause"{"*".into()}else{args["threadId"].as_i64().and_then(|id|self.wire_thread(id)).unwrap_or_else(||"*".into())};let wire_command=match command.as_str(){"continue"=>106,"next"=>108,"stepIn" if self.just_my_code=>144,"stepIn"=>107,"stepOut"=>109,"rustpyForceStepInto"=>107,_=>105};self.send(wire_command,&thread);self.response(&request,json!({"allThreadsContinued":true}),None);if command!="pause"{self.refs.clear();self.frames.clear();self.stacks.clear();self.event("continued",json!({"allThreadsContinued":true}));}},
   "exceptionInfo"=>{let thread=args["threadId"].as_i64().and_then(|id|self.wire_thread(id)).unwrap_or_else(||"*".into());self.pending(request,155,&thread,Reply::Exception);},
   "source"=>{let path=args["source"]["path"].as_str().unwrap_or("");if field(path){self.pending(request,132,path,Reply::Source);}else{self.response(&request,json!({}),Some("Invalid source path"));}},
   "disconnect"=>{self.send(if args["terminateDebuggee"].as_bool().unwrap_or(false){129}else{106},"*");self.response(&request,json!({}),None);self.event("terminated",json!({}));self.finished=true;},
   "pydevdSystemInfo"=>self.response(&request,json!({"pydevd":{"implementation":"pydevd legacy quoted-line","usingCython":null,"usingFrameEval":null}}),None),
   _=>self.response(&request,json!({}),Some("Request is not supported by the legacy pydevd bridge")),
  }
    }
    fn set_breakpoints(&mut self, request: &Value, args: &Value) {
        let path = args["source"]["path"].as_str().unwrap_or("");
        if !field(path) {
            self.response(request, json!({}), Some("Invalid breakpoint path"));
            return;
        }
        if let Some((kind, ids)) = self.breakpoints.remove(path) {
            for id in ids {
                self.send(112, &format!("{kind}\t{path}\t{id}"));
            }
        }
        let python = std::path::Path::new(path)
            .extension()
            .is_some_and(|ext| ext == "py" || ext == "pyw");
        let kind = if !python && self.django {
            "django-line"
        } else if !python && self.jinja {
            "jinja2-line"
        } else {
            "python-line"
        };
        let mut result = Vec::new();
        let mut ids = Vec::new();
        if let Some(points) = args["breakpoints"].as_array() {
            for point in points.iter().take(10000) {
                let line = point["line"].as_u64().unwrap_or(1);
                let id = self.id();
                let condition = markers(point["condition"].as_str().unwrap_or("None"));
                let hit = markers(point["hitCondition"].as_str().unwrap_or("None"));
                let log = point["logMessage"].as_str();
                let expression = log.map(log_expression).unwrap_or_else(|| "None".into());
                self.send(111,&format!("{id}\t{kind}\t{path}\t{line}\tNone\t{condition}\t{expression}\t{hit}\t{}\tALL",if log.is_some(){"True"}else{"False"}));
                ids.push(id);
                result.push(json!({"id":id,"line":line,"verified":false,"message":"Legacy pydevd installs breakpoints without executable-line verification"}));
            }
        }
        self.breakpoints.insert(path.into(), (kind.into(), ids));
        self.response(request, json!({"breakpoints":result}), None);
    }
    pub fn expire(&mut self) {
        let expired = self
            .pending
            .iter()
            .filter(|(_, pending)| Instant::now() >= pending.deadline)
            .map(|(seq, _)| *seq)
            .collect::<Vec<_>>();
        for seq in expired {
            if let Some(pending) = self.pending.remove(&seq) {
                self.response(
                    &pending.request,
                    json!({}),
                    Some("Legacy debugger request timed out"),
                );
            }
        }
    }
    pub fn message(&mut self, line: &str) {
        let mut fields = line.splitn(3, '\t');
        let command = fields
            .next()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);
        let seq = fields
            .next()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        let payload = decode(fields.next().unwrap_or(""));
        if let Some(pending) = self.pending.remove(&seq) {
            if command == 901 {
                self.response(&pending.request, json!({}), Some(&payload));
                return;
            }
            self.reply(pending, &payload);
            return;
        }
        match command {
            103 => {
                if let Ok(xml) = xml(&payload) {
                    for node in xml.descendants().filter(|node| node.has_tag_name("thread")) {
                        let wire = attr(node, "id");
                        let id = self.thread(&wire);
                        self.event("thread", json!({"reason":"started","threadId":id}));
                    }
                }
            }
            104 => {
                if let Some(id) = self.threads.remove(&payload) {
                    self.event("thread", json!({"reason":"exited","threadId":id}));
                }
                if self.threads.is_empty() {
                    self.event("terminated", json!({}));
                }
            }
            105 => {
                if let Ok(xml) = xml(&payload) {
                    if let Some(node) = xml.descendants().find(|node| node.has_tag_name("thread")) {
                        let wire = attr(node, "id");
                        let id = self.thread(&wire);
                        self.capture_stack(&wire, &xml);
                        let reason = match node.attribute("stop_reason").unwrap_or("") {
                            "122" | "131" | "137" => "exception",
                            "107" | "108" | "109" | "144" => "step",
                            "105" => "pause",
                            _ => "breakpoint",
                        };
                        self.event("stopped",json!({"threadId":id,"reason":reason,"description":attr(node,"message"),"allThreadsStopped":true}));
                    }
                }
            }
            106 => self.event("continued", json!({"allThreadsContinued":true})),
            116 => {
                if let Ok(xml) = xml(&payload) {
                    for node in xml.descendants().filter(|node| node.has_tag_name("io")) {
                        self.event("output",json!({"category":if node.attribute("ctx")==Some("2"){"stderr"}else{"stdout"},"output":attr(node,"s")}));
                    }
                }
            }
            150 => self.event(
                "output",
                json!({"category":"console","output":format!("Cython acceleration: {payload}\n")}),
            ),
            149 => {
                self.send(159, "");
            }
            _ => {}
        }
    }
    fn capture_stack(&mut self, thread: &str, xml: &roxmltree::Document<'_>) -> Vec<Value> {
        let mut frames = Vec::new();
        for node in xml
            .descendants()
            .filter(|node| node.has_tag_name("frame"))
            .take(10000)
        {
            let id = self.id();
            self.frames.insert(id, (thread.into(), attr(node, "id")));
            frames.push(json!({"id":id,"name":attr(node,"name"),"line":node.attribute("line").and_then(|line|line.parse::<usize>().ok()).unwrap_or(1),"column":1,"source":{"path":attr(node,"file")}}));
        }
        self.stacks.insert(thread.into(), frames.clone());
        frames
    }
    fn variable(
        &mut self,
        node: roxmltree::Node<'_, '_>,
        parent: &Reference,
        name_override: Option<&str>,
    ) -> Value {
        let name = attr(node, "name");
        let key = name_override.unwrap_or(&name);
        let attrs = if parent.attrs.is_empty() {
            key.to_owned()
        } else {
            format!("{}\t{key}", parent.attrs)
        };
        let reference = if node.attribute("isContainer") == Some("True") && field(key) {
            self.reference(Reference {
                attrs,
                ..parent.clone()
            })
        } else {
            0
        };
        let mut variable = json!({"name":name,"value":attr(node,"value"),"type":attr(node,"type"),"variablesReference":reference});
        if let Some(expression) = evaluate_name(parent, key) {
            variable["evaluateName"] = expression.into();
        }
        variable
    }
    fn reply(&mut self, pending: Pending, payload: &str) {
        if matches!(&pending.kind, Reply::Source) {
            self.response(
                &pending.request,
                json!({"content":payload,"mimeType":"text/x-python"}),
                None,
            );
            return;
        }
        let Ok(xml) = xml(payload) else {
            self.response(
                &pending.request,
                json!({}),
                Some("Invalid legacy debugger XML response"),
            );
            return;
        };
        let body = match pending.kind {
            Reply::Threads => {
                let mut threads = Vec::new();
                for node in xml.descendants().filter(|node| node.has_tag_name("thread")) {
                    let wire = attr(node, "id");
                    let id = self.thread(&wire);
                    threads.push(json!({"id":id,"name":attr(node,"name")}));
                }
                json!({"threads":threads})
            }
            Reply::Stack { thread } => {
                let frames = self.capture_stack(&thread, &xml);
                let count = frames.len();
                let start = pending.request["arguments"]["startFrame"]
                    .as_u64()
                    .unwrap_or(0) as usize;
                let levels = pending.request["arguments"]["levels"]
                    .as_u64()
                    .unwrap_or(100)
                    .clamp(1, 1000) as usize;
                json!({"totalFrames":count,"stackFrames":frames.into_iter().skip(start).take(levels).collect::<Vec<_>>()})
            }
            Reply::Variables {
                reference,
                start,
                count,
            } => {
                json!({"variables":xml.descendants().filter(|node|node.has_tag_name("var")).skip(start).take(count).map(|node|self.variable(node,&reference,None)).collect::<Vec<_>>()})
            }
            Reply::Evaluate { reference, temp } => {
                let Some(node) = xml.descendants().find(|node| node.has_tag_name("var")) else {
                    self.response(
                        &pending.request,
                        json!({}),
                        Some("Legacy evaluation returned no value"),
                    );
                    return;
                };
                if node.attribute("isErrorOnEval") == Some("True") {
                    self.response(&pending.request, json!({}), Some(&attr(node, "value")));
                    return;
                }
                let variable = self.variable(node, &reference, Some(&temp));
                json!({"result":variable["value"],"type":variable["type"],"variablesReference":variable["variablesReference"]})
            }
            Reply::SetValue { reference, name } => {
                let Some(node) = xml.descendants().find(|node| node.has_tag_name("var")) else {
                    self.response(
                        &pending.request,
                        json!({}),
                        Some("Legacy value update returned no value"),
                    );
                    return;
                };
                self.variable(node, &reference, Some(&name))
            }
            Reply::Completions => {
                json!({"targets":xml.descendants().filter(|node|node.has_tag_name("comp")).take(1000).map(|node|json!({"label":attr(node,"p0"),"type":"property"})).collect::<Vec<_>>()})
            }
            Reply::Exception => {
                json!({"exceptionId":"Python exception","breakMode":"unhandled","description":xml.descendants().filter_map(|node|node.text()).collect::<Vec<_>>().join(" "),"details":{"stackTrace":payload}})
            }
            Reply::Source => unreachable!(),
        };
        self.response(&pending.request, body, None);
    }
}
fn xml(text: &str) -> Result<roxmltree::Document<'_>, roxmltree::Error> {
    roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 100000,
            ..Default::default()
        },
    )
}
fn attr(node: roxmltree::Node<'_, '_>, name: &str) -> String {
    decode(node.attribute(name).unwrap_or(""))
}
fn field(text: &str) -> bool {
    !text.contains(['\r', '\n', '\t'])
}
fn markers(text: &str) -> String {
    text.replace('\n', "@_@NEW_LINE_CHAR@_@")
        .replace('\t', "@_@TAB_CHAR@_@")
}
fn decode(text: &str) -> String {
    fn hex(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                output.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}
fn log_expression(text: &str) -> String {
    let mut parts = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        if start > 0 {
            parts.push(serde_json::to_string(&rest[..start]).unwrap());
        }
        if let Some(end) = rest[start + 1..].find('}') {
            let expression = &rest[start + 1..start + 1 + end];
            parts.push(format!("str(({expression}))"));
            rest = &rest[start + end + 2..];
        } else {
            parts.push(serde_json::to_string(&rest[start..]).unwrap());
            rest = "";
            break;
        }
    }
    if !rest.is_empty() {
        parts.push(serde_json::to_string(rest).unwrap());
    }
    if parts.is_empty() {
        parts.push("''".into());
    }
    format!("print({})", parts.join(" + "))
}

fn evaluate_name(parent: &Reference, name: &str) -> Option<String> {
    let mut parts = parent
        .attrs
        .split('\t')
        .filter(|part| !part.is_empty())
        .chain(std::iter::once(name));
    let mut expression = parts.next()?.to_owned();
    if !expression
        .chars()
        .all(|character| character.is_alphanumeric() || character == '_')
    {
        return None;
    }
    for part in parts {
        if part.starts_with('[') && part.ends_with(']') {
            expression.push_str(part);
        } else if part.parse::<usize>().is_ok() || part.starts_with(['\'', '"']) {
            expression.push_str(&format!("[{part}]"));
        } else if part
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_')
        {
            expression.push('.');
            expression.push_str(part);
        } else {
            return None;
        }
    }
    Some(expression)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(seq: u64, command: &str, args: Value) -> Value {
        json!({"seq":seq,"type":"request","command":command,"arguments":args})
    }
    #[test]
    fn attach_waits_for_legacy_peer_and_configuration_done() {
        let mut state = State::default();
        state.request(request(1, "initialize", json!({})));
        state.request(request(2, "attach", json!({"justMyCode":true})));
        assert!(!state
            .dap
            .iter()
            .any(|message| message["event"] == "initialized"));
        state.connected();
        assert!(state
            .dap
            .iter()
            .any(|message| message["event"] == "initialized"));
        assert!(!state.dap.iter().any(|message| message["request_seq"] == 2));
        state.request(request(3, "configurationDone", json!({})));
        assert!(state.wire.iter().any(|line| line.starts_with("101\t")));
        assert!(state
            .dap
            .iter()
            .any(|message| message["request_seq"] == 2 && message["success"] == true));
    }
    #[test]
    fn stopped_frames_become_real_scope_and_paged_variable_references() {
        let mut state = State::default();
        state.message("105\t2\t<xml><thread id=\"pid_1\" stop_reason=\"111\"><frame id=\"frame_7\" name=\"work\" file=\"/srv/app.py\" line=\"12\"/></thread></xml>");
        let frame = *state.frames.keys().next().unwrap();
        state.request(request(1, "scopes", json!({"frameId":frame})));
        let reference = state.dap.last().unwrap()["body"]["scopes"][0]["variablesReference"]
            .as_i64()
            .unwrap();
        state.request(request(
            2,
            "variables",
            json!({"variablesReference":reference,"start":1,"count":1}),
        ));
        let seq = *state.pending.keys().next().unwrap();
        state.message(&format!("114\t{seq}\t<xml><var name=\"first\" value=\"1\" type=\"int\"/><var name=\"second\" value=\"%CE%BB\" type=\"str\"/></xml>"));
        let variables = &state.dap.last().unwrap()["body"]["variables"];
        assert_eq!(variables.as_array().unwrap().len(), 1);
        assert_eq!(variables[0]["name"], "second");
        assert_eq!(variables[0]["value"], "λ");
    }
    #[test]
    fn code_fragments_do_not_inject_protocol_lines_and_detach_never_exits() {
        let mut state = State::default();
        state.frames.insert(1, ("thread".into(), "frame".into()));
        state.request(request(
            1,
            "evaluate",
            json!({"frameId":1,"context":"repl","expression":"x = 1\ny = '\t'"}),
        ));
        assert_eq!(
            state.wire[0].bytes().filter(|byte| *byte == b'\n').count(),
            1
        );
        assert!(state.wire[0].contains("__import__('ast').parse"));
        state.wire.clear();
        state.request(request(2, "disconnect", json!({"terminateDebuggee":false})));
        assert!(state.wire.iter().any(|line| line.starts_with("106\t")));
        assert!(!state.wire.iter().any(|line| line.starts_with("129\t")));
    }
    #[test]
    fn template_breakpoint_removal_preserves_original_plugin_kind() {
        let mut state = State::default();
        state.request(request(1, "attach", json!({"django":true})));
        state.request(request(
            2,
            "setBreakpoints",
            json!({"source":{"path":"/app/templates/page.html"},"breakpoints":[{"line":3}]}),
        ));
        assert!(state
            .wire
            .iter()
            .any(|line| line.contains("django-line\t/app/templates/page.html\t3")));
        state.django = false;
        state.jinja = true;
        state.request(request(
            3,
            "setBreakpoints",
            json!({"source":{"path":"/app/templates/page.html"},"breakpoints":[{"line":4}]}),
        ));
        assert!(state
            .wire
            .iter()
            .any(|line| line.starts_with("112\t") && line.contains("django-line\t")));
        assert!(state
            .wire
            .iter()
            .any(|line| line.starts_with("111\t") && line.contains("jinja2-line\t")));
        state.request(request(
            4,
            "setBreakpoints",
            json!({"source":{"path":"/app/main.py"},"breakpoints":[{"line":1}]}),
        ));
        assert!(state
            .wire
            .last()
            .unwrap()
            .contains("python-line\t/app/main.py"));
    }

    #[test]
    fn invalid_percent_sequences_and_xml_entities_remain_bounded() {
        assert_eq!(decode("%λ + %20"), "%λ +  ");
        assert!(xml("<!DOCTYPE a [<!ENTITY v 'expanded'>]><xml>&v;</xml>").is_err());
    }
}
