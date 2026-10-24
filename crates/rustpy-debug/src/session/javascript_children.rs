//! js-debug's standalone server asks the client to open a second DAP connection
//! for each target. References and request sequences belong to that connection.
use super::*;

const MAX_TARGETS: usize = 16;

#[derive(Default)]
pub(super) struct Children {
    pub(super) launch: Option<(String, Value)>,
    pub(super) handshake_failed: bool,
    pub(super) launch_acknowledged: bool,
    pub(super) configuration_acknowledged: bool,
    parents: Vec<DebugSession>,
    pending: Vec<ChildSession>,
    targets: HashSet<String>,
    passive: bool,
    stopped: bool,
}

impl Children {
    pub(super) fn is_stopping(&self) -> bool {
        self.stopped
    }
    pub(super) fn transport_cleanup_ids(&self) -> Vec<u64> {
        self.parents
            .iter()
            .flat_map(DebugSession::transport_cleanup_ids)
            .chain(
                self.pending
                    .iter()
                    .flat_map(|child| child.session.transport_cleanup_ids()),
            )
            .collect()
    }
    pub(super) fn disconnect_completed(&self) -> bool {
        self.pending.is_empty() && self.parents.iter().all(DebugSession::disconnect_completed)
    }
}

struct ChildSession {
    session: Box<DebugSession>,
    reverse_seq: u64,
    // None means the foreground connection; otherwise an index into parents.
    owner: Option<usize>,
    deadline: Instant,
}

impl DebugSession {
    pub(super) fn start_javascript_child(&mut self, seq: u64, arguments: &Value) -> bool {
        if !self.is_javascript_adapter() {
            return false;
        }
        let admitted = (|| -> anyhow::Result<DebugSession> {
            anyhow::ensure!(
                !self.javascript_children.stopped && self.is_active(),
                "Debug session is stopping"
            );
            anyhow::ensure!(
                !self
                    .javascript_children
                    .pending
                    .iter()
                    .any(|child| child.owner.is_none() && child.reverse_seq == seq),
                "Duplicate reverse request sequence"
            );
            anyhow::ensure!(
                self.javascript_children.targets.len() < MAX_TARGETS,
                "Too many JavaScript debug targets"
            );
            let request = arguments["request"].as_str().unwrap_or("");
            anyhow::ensure!(
                matches!(request, "launch" | "attach"),
                "Unsupported child debug request"
            );
            let configuration = &arguments["configuration"];
            anyhow::ensure!(configuration.is_object(), "Missing child configuration");
            let target = configuration["__pendingTargetId"]
                .as_str()
                .filter(|id| !id.is_empty() && id.len() <= 4096)
                .ok_or_else(|| anyhow::anyhow!("Missing child target identity"))?;
            anyhow::ensure!(
                !self.javascript_children.targets.contains(target),
                "Duplicate child target identity"
            );
            let (host, port, adapter) = self
                .protocol
                .endpoint
                .clone()
                .ok_or_else(|| anyhow::anyhow!("Missing admitted adapter endpoint"))?;
            anyhow::ensure!(
                adapter == "javascript",
                "Child request is not from a JavaScript adapter"
            );
            // Never accept a host/port supplied by the reverse request.
            let mut child = Self::attach_transport(
                &host,
                port,
                self.options.clone(),
                self.breakpoints.clone(),
                &adapter,
                false,
                Some(self._port_lease.shared()),
            )?;
            child.id = self.id;
            child.name = self.name.clone();
            child.config = self.config.clone();
            child.exceptions = self.exceptions.clone();
            child.muted = self.muted;
            child.watches = self
                .watches
                .iter()
                .map(|watch| Watch {
                    expression: watch.expression.clone(),
                    result: None,
                    error: None,
                })
                .collect();
            child.stop_epoch = self.stop_epoch.saturating_add(1);
            child.protocol.function_breakpoints = self.protocol.function_breakpoints.clone();
            child.protocol.data_breakpoints = self.protocol.data_breakpoints.clone();
            child.javascript_children.passive = true;
            child.javascript_children.targets = self.javascript_children.targets.clone();
            child.javascript_children.targets.insert(target.to_string());
            child.javascript_children.launch = Some((request.to_string(), configuration.clone()));
            self.javascript_children.targets.insert(target.to_string());
            Ok(child)
        })();
        match admitted {
            Ok(child) => self.javascript_children.pending.push(ChildSession {
                session: Box::new(child),
                reverse_seq: seq,
                owner: None,
                deadline: Instant::now() + Duration::from_secs(30),
            }),
            Err(error) => {
                self.errors
                    .push(format!("Cannot start JavaScript target: {error}"));
                if let Some(client) = &mut self.client {
                    let _ = client.respond(seq, "startDebugging", false, json!({}));
                }
            }
        }
        true
    }

    pub(super) fn poll_javascript_children(&mut self) -> bool {
        if self.javascript_children.passive {
            return false;
        }
        let mut tree = std::mem::take(&mut self.javascript_children);
        let mut changed = false;
        for (index, parent) in tree.parents.iter_mut().enumerate() {
            parent.javascript_children.targets = tree.targets.clone();
            parent.breakpoints = self.breakpoints.clone();
            parent.exceptions = self.exceptions.clone();
            parent.muted = self.muted;
            parent.stop_epoch = self.stop_epoch;
            changed |= parent.poll();
            for mut child in parent.javascript_children.pending.drain(..) {
                child.owner = Some(index);
                tree.pending.push(child);
            }
            tree.targets
                .extend(parent.javascript_children.targets.iter().cloned());
            self.output.append(&mut parent.output);
            self.errors.append(&mut parent.errors);
        }
        let mut ready = None;
        let mut waiting = Vec::new();
        for mut child in tree.pending.drain(..) {
            child.session.javascript_children.targets = tree.targets.clone();
            changed |= child.session.poll();
            let owner_active = child.owner.map_or_else(
                || self.is_active(),
                |index| tree.parents.get(index).is_some_and(DebugSession::is_active),
            );
            let failed = !owner_active
                || matches!(
                    child.session.state,
                    DebugState::Failed(_) | DebugState::Terminated(_)
                )
                || child.session.javascript_children.handshake_failed
                || Instant::now() >= child.deadline
                || tree.stopped;
            // Both acknowledgements are required. js-debug resolves child launch
            // in initAdapter, independently of the parent's reverse response.
            let complete = child.session.javascript_children.launch_acknowledged
                && (child.session.javascript_children.configuration_acknowledged
                    || !child
                        .session
                        .protocol
                        .supports("supportsConfigurationDoneRequest"))
                && child.session.config_done
                && !child.session.pending.values().any(|pending| {
                    matches!(
                        pending,
                        Pending::Initialize
                            | Pending::Attach
                            | Pending::ConfigurationDone
                            | Pending::SetBreakpoints(..)
                            | Pending::SetExceptionBreakpoints
                    )
                });
            if failed || (complete && ready.is_none()) {
                let owner = child.owner.and_then(|index| tree.parents.get_mut(index));
                let client = match owner {
                    Some(parent) => &mut parent.client,
                    None => &mut self.client,
                };
                let acknowledged = client.as_mut().is_some_and(|client| {
                    client
                        .respond(child.reverse_seq, "startDebugging", !failed, json!({}))
                        .is_ok()
                });
                changed = true;
                if !failed && acknowledged {
                    ready = Some(child);
                } else {
                    child.session.stop();
                    self.output.append(&mut child.session.output);
                    self.errors.append(&mut child.session.errors);
                    self.errors.push(
                        "JavaScript child handshake failed, expired, or its parent disconnected"
                            .into(),
                    );
                }
            } else {
                waiting.push(child);
            }
        }
        tree.pending = waiting;
        if let Some(mut child) = ready {
            // Keep the complete parent state and its pending sequence map alive;
            // UI requests now address the child's independent reference namespace.
            child
                .session
                .rebase_javascript_epoch(self.stop_epoch.saturating_add(1));
            let parent_index = tree.parents.len();
            for pending in &mut tree.pending {
                if pending.owner.is_none() {
                    pending.owner = Some(parent_index);
                }
            }
            let mut output = std::mem::take(&mut self.output);
            output.append(&mut child.session.output);
            child.session.output = output;
            std::mem::swap(self, &mut child.session);
            child.session.javascript_children = Children {
                passive: true,
                ..Default::default()
            };
            tree.parents.push(*child.session);
            // Nested requests that arrived during initialization retain their owner.
            tree.pending.append(&mut self.javascript_children.pending);
            tree.targets
                .extend(self.javascript_children.targets.drain());
            tree.launch = self.javascript_children.launch.take();
            changed = true;
        }
        self.javascript_children = tree;
        changed
    }

    fn rebase_javascript_epoch(&mut self, minimum: u64) {
        if self.stop_epoch >= minimum {
            return;
        }
        let old = self.stop_epoch;
        self.stop_epoch = minimum;
        for pending in self.pending.values_mut() {
            let epoch = match pending {
                Pending::BackendCommand { epoch, .. }
                | Pending::StackTrace { epoch, .. }
                | Pending::Scopes { epoch, .. }
                | Pending::SetVariable { epoch, .. }
                | Pending::Completions { epoch, .. }
                | Pending::BackendView { epoch, .. }
                | Pending::VariablesPage { epoch, .. }
                | Pending::Evaluate(_, epoch)
                | Pending::StepInTargets(epoch) => Some(epoch),
                Pending::Protocol { epoch, .. } => epoch.as_mut(),
                _ => None,
            };
            if let Some(epoch) = epoch {
                if *epoch == old {
                    *epoch = minimum;
                }
            }
        }
        for item in &mut self.repl {
            if item.epoch == old {
                item.epoch = minimum;
            }
        }
        for result in [&mut self.hover_result, &mut self.dialog_result]
            .into_iter()
            .flatten()
        {
            if result.epoch == old {
                result.epoch = minimum;
            }
        }
    }

    pub(super) fn stop_javascript_children(&mut self) {
        if self.javascript_children.stopped {
            return;
        }
        self.javascript_children.stopped = true;
        let mut pending = std::mem::take(&mut self.javascript_children.pending);
        for child in &mut pending {
            let owner = child
                .owner
                .and_then(|index| self.javascript_children.parents.get_mut(index));
            let client = match owner {
                Some(parent) => &mut parent.client,
                None => &mut self.client,
            };
            if let Some(client) = client {
                let _ = client.respond(child.reverse_seq, "startDebugging", false, json!({}));
            }
            child.session.stop();
        }
        for parent in &mut self.javascript_children.parents {
            parent.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    fn read_frame(stream: &mut TcpStream) -> Value {
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            header.push(byte[0]);
        }
        let header = String::from_utf8(header).unwrap();
        let length: usize = header
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .unwrap()
            .parse()
            .unwrap();
        let mut body = vec![0; length];
        stream.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn response(request: &Value, body: Value) -> Message {
        Message::Response {
            request_seq: request["seq"].as_u64().unwrap(),
            command: request["command"].as_str().unwrap().into(),
            success: true,
            message: None,
            body,
        }
    }

    fn accept(listener: &TcpListener) -> TcpStream {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match listener.accept() {
                Ok((stream, _)) => return stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Err(error) => panic!("admitted DAP client did not connect: {error}"),
            }
        }
    }

    fn root() -> (DebugSession, TcpListener, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let session = DebugSession::attach_adapter(
            "127.0.0.1",
            listener.local_addr().unwrap().port(),
            DebugOptions::default(),
            HashMap::new(),
            "javascript",
        )
        .unwrap();
        let mut peer = accept(&listener);
        let initialize = read_frame(&mut peer);
        assert_eq!(
            initialize["arguments"]["supportsStartDebuggingRequest"],
            true
        );
        let mut session = session;
        session.config.type_options =
            Some(rustpy_project::configuration_types::ConfigurationSpec::new(
                rustpy_project::configuration_types::ConfigurationType::JavaScriptDebug,
            ));
        session.id = 7182;
        session.name = "Owned JavaScript session".into();
        (session, listener, peer)
    }

    #[test]
    fn child_handshake_preserves_parent_and_acknowledges_only_complete_target() {
        let (mut session, listener, mut parent) = root();
        let configuration =
            json!({"type":"pwa-node", "name":"owned target", "__pendingTargetId":"target-42"});
        assert!(session.start_javascript_child(
            99,
            &json!({"request":"attach", "configuration":configuration})
        ));
        let mut peer = accept(&listener);
        let initialize = read_frame(&mut peer);
        let child = &mut session.javascript_children.pending[0].session;
        child.handle(response(
            &initialize,
            json!({"supportsConfigurationDoneRequest":true}),
        ));
        let launch = read_frame(&mut peer);
        assert_eq!(launch["command"], "attach");
        assert_eq!(
            launch["arguments"], configuration,
            "reverse configuration must not acquire the root program/env"
        );
        child.handle(Message::Event {
            event: "initialized".into(),
            body: json!({}),
        });
        let mut configuration_requests = Vec::new();
        loop {
            let request = read_frame(&mut peer);
            let done = request["command"] == "configurationDone";
            configuration_requests.push(request);
            if done {
                break;
            }
        }
        for request in &configuration_requests {
            child.handle(response(request, json!({})));
        }
        session.poll();
        assert_eq!(
            session.javascript_children.parents.len(),
            0,
            "configuration alone must not acknowledge the reverse request"
        );
        parent.set_nonblocking(true).unwrap();
        assert_eq!(
            parent.peek(&mut [0; 1]).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        parent.set_nonblocking(false).unwrap();
        session.javascript_children.pending[0]
            .session
            .handle(response(&launch, json!({})));
        session.poll();
        let acknowledgement = read_frame(&mut parent);
        assert_eq!(acknowledgement["request_seq"], 99);
        assert_eq!(acknowledgement["success"], true);
        assert_eq!(session.id, 7182);
        assert_eq!(session.name, "Owned JavaScript session");
        assert_eq!(session.javascript_children.parents.len(), 1);
        assert!(
            session.javascript_children.parents[0]
                .pending
                .values()
                .any(|pending| matches!(pending, Pending::Initialize)),
            "parent sequence namespace remains retained"
        );
        session.state = DebugState::Stopped {
            thread_id: 1,
            reason: "breakpoint".into(),
            description: String::new(),
        };
        session.request_variables(42);
        let variables = read_frame(&mut peer);
        assert_eq!(variables["command"], "variables");
        assert_eq!(variables["arguments"]["variablesReference"], 42);
        session.stop();
        assert_eq!(read_frame(&mut peer)["command"], "disconnect");
        assert_eq!(read_frame(&mut parent)["command"], "disconnect");
    }

    #[test]
    fn stopped_child_is_rejected_and_cannot_be_revived_by_late_success() {
        let (mut session, listener, mut parent) = root();
        session.start_javascript_child(8, &json!({"request":"launch", "configuration":{"type":"pwa-node", "__pendingTargetId":"late"}}));
        let mut peer = accept(&listener);
        let initialize = read_frame(&mut peer);
        session.stop();
        let rejection = read_frame(&mut parent);
        assert_eq!(rejection["request_seq"], 8);
        assert_eq!(rejection["success"], false);
        assert!(session.javascript_children.pending.is_empty());
        // A genuine late wire response on the now discarded child cannot change
        // the foreground session or admit another target after Stop.
        let bytes = serde_json::to_vec(&json!({"seq":8,"type":"response","request_seq":initialize["seq"],"command":"initialize","success":true,"body":{}})).unwrap();
        let _ = write!(peer, "Content-Length: {}\r\n\r\n", bytes.len());
        let _ = peer.write_all(&bytes);
        session.poll();
        session.start_javascript_child(
            9,
            &json!({"request":"launch", "configuration":{"__pendingTargetId":"new"}}),
        );
        assert!(session.javascript_children.pending.is_empty());
        assert!(matches!(session.state, DebugState::Terminated(_)));
    }
    #[test]
    fn malformed_duplicate_and_expired_targets_receive_negative_replies() {
        let (mut session, listener, mut parent) = root();
        session.start_javascript_child(
            10,
            &json!({"request":"run", "configuration":{"__pendingTargetId":"bad"}}),
        );
        assert_eq!(read_frame(&mut parent)["success"], false);
        let request = json!({"request":"launch", "configuration":{"__pendingTargetId":"unique"}});
        session.start_javascript_child(11, &request);
        let mut peer = accept(&listener);
        let _ = read_frame(&mut peer);
        session.start_javascript_child(12, &request);
        let duplicate = read_frame(&mut parent);
        assert_eq!(duplicate["request_seq"], 12);
        assert_eq!(duplicate["success"], false);
        assert_eq!(session.javascript_children.pending.len(), 1);
        session.javascript_children.pending[0].deadline = Instant::now();
        session.poll();
        let expired = read_frame(&mut parent);
        assert_eq!(expired["request_seq"], 11);
        assert_eq!(expired["success"], false);
        assert!(session.javascript_children.pending.is_empty());
        assert!(session.javascript_children.parents.is_empty());
    }
}
