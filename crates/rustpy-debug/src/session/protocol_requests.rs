use super::*;
impl DebugSession {
    fn optional_request(
        &mut self,
        capability: &str,
        command: &str,
        args: Value,
        epoch: bool,
        key: Option<i64>,
    ) {
        if !capability.is_empty() && !self.protocol.supports(capability) {
            self.errors
                .push(format!("The adapter does not support {command}"));
            return;
        }
        self.request(
            command,
            args,
            Pending::Protocol {
                command: command.into(),
                epoch: epoch.then_some(self.stop_epoch),
                key,
            },
        );
    }
    pub fn request_modules(&mut self) {
        self.optional_request(
            "supportsModulesRequest",
            "modules",
            json!({"startModule":0,"moduleCount":1000}),
            false,
            None,
        );
    }
    pub fn request_loaded_sources(&mut self) {
        self.optional_request(
            "supportsLoadedSourcesRequest",
            "loadedSources",
            json!({}),
            false,
            None,
        );
    }
    pub fn request_source(&mut self, source: Value) {
        let reference = source["sourceReference"].as_i64().unwrap_or(0);
        if reference <= 0 {
            self.errors
                .push("Source has no adapter reference; open its local path instead".into());
            return;
        }
        self.protocol.source_open = Some(reference);
        self.optional_request(
            "",
            "source",
            json!({"source":source,"sourceReference":reference}),
            false,
            Some(reference),
        );
    }
    pub fn request_frame_source(&mut self, frame: i64) {
        if let Some(source) = self.protocol.frame_sources.get(&frame).cloned() {
            self.request_source(source);
        }
    }
    pub fn set_expression(&mut self, expression: &str, value: &str) {
        let Some(frame) = self.frames.get(self.selected_frame) else {
            return;
        };
        self.optional_request(
            "supportsSetExpression",
            "setExpression",
            json!({"expression":expression,"value":value,"frameId":frame.id}),
            true,
            None,
        );
    }
    pub fn set_function_breakpoints(&mut self, breakpoints: Vec<Value>) {
        self.protocol.function_breakpoints = breakpoints.clone();
        self.optional_request(
            "supportsFunctionBreakpoints",
            "setFunctionBreakpoints",
            json!({"breakpoints":if self.muted {Vec::new()}else{breakpoints}}),
            false,
            None,
        );
    }
    pub fn request_data_breakpoint(&mut self, name: &str, reference: i64) {
        self.protocol.custom_results.remove("dataBreakpointInfo");
        self.optional_request("supportsDataBreakpoints","dataBreakpointInfo",json!({"name":name,"variablesReference":reference,"frameId":self.frames.get(self.selected_frame).map(|frame|frame.id)}),true,None);
    }
    pub fn set_data_breakpoints(&mut self, breakpoints: Vec<Value>) {
        self.protocol.data_breakpoints = breakpoints.clone();
        self.optional_request(
            "supportsDataBreakpoints",
            "setDataBreakpoints",
            json!({"breakpoints":if self.muted{Vec::new()}else{breakpoints}}),
            false,
            None,
        );
    }
    pub fn restart_selected_frame(&mut self) {
        if let Some(frame) = self.frames.get(self.selected_frame) {
            self.optional_request(
                "supportsRestartFrame",
                "restartFrame",
                json!({"frameId":frame.id}),
                true,
                None,
            );
        }
    }
    pub fn terminate(&mut self) {
        self.optional_request(
            "supportsTerminateRequest",
            "terminate",
            json!({"restart":false}),
            false,
            None,
        );
    }
    pub fn debugger_system_info(&mut self) {
        self.optional_request("", "pydevdSystemInfo", json!({}), false, None);
    }
    pub fn set_debugger_property(&mut self, properties: Value) {
        self.optional_request(
            "supportsDebuggerProperties",
            "setDebuggerProperty",
            properties,
            false,
            None,
        );
    }
    pub fn configure_attach_options(&mut self, properties: Value) -> anyhow::Result<()> {
        anyhow::ensure!(
            properties.is_object(),
            "Debugger options must be a JSON object"
        );
        use rustpy_project::configuration_types::{ConfigurationSpec, ConfigurationType};
        if self.config.type_options.is_none() {
            let mut spec = match &self.config.kind {
                rustpy_project::RunConfigKind::Python { script } => {
                    let mut spec = ConfigurationSpec::new(ConfigurationType::Python);
                    spec.target = script.display().to_string();
                    spec
                }
                rustpy_project::RunConfigKind::PythonModule { module } => {
                    let mut spec = ConfigurationSpec::new(ConfigurationType::PythonModule);
                    spec.target = module.clone();
                    spec
                }
                _ => ConfigurationSpec::default(),
            };
            spec.debug_options = properties.clone();
            self.config.type_options = Some(spec);
        } else if let Some(spec) = &mut self.config.type_options {
            spec.debug_options = properties.clone();
        }
        self.protocol.variable_presentation = properties;
        Ok(())
    }
    pub fn set_source_maps(&mut self, path: &str, mappings: Value) {
        if !mappings.is_array() {
            self.errors
                .push("Source mappings must be a JSON array".into());
            return;
        }
        self.optional_request(
            "",
            "setPydevdSourceMap",
            json!({"source":{"path":path},"pydevdSourceMaps":mappings}),
            false,
            None,
        );
    }
    pub fn authorize_pydevd(&mut self, token: &str) {
        self.optional_request(
            "",
            "pydevdAuthorize",
            json!({"debugServerAccessToken":token}),
            false,
            None,
        );
    }
    pub fn reconnect_adapter(&mut self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.child.is_none(),
            "Restart the owned debuggee instead of reconnecting its transport"
        );
        let (host, port, adapter) = self
            .protocol
            .endpoint
            .clone()
            .ok_or_else(|| anyhow::anyhow!("No reconnectable adapter endpoint"))?;
        self.client = None;
        self.pending.clear();
        self.protocol.deadlines.clear();
        self.end_suspension();
        self.initialized = false;
        self.config_done = false;
        self.bp_requests_in_flight = 0;
        self.protocol.negotiated = false;
        let address = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        let reverse = self
            .config
            .type_options
            .as_ref()
            .is_some_and(|spec| spec.pydevd && spec.listen);
        let mut client = if reverse {
            DapClient::listen(&address, Duration::from_secs(86_400))?
        } else {
            DapClient::connect(&address, Duration::from_secs(20))?
        };
        let seq=client.request("initialize",json!({"clientID":"rustpy","adapterID":adapter,"pathFormat":"path","linesStartAt1":true,"columnsStartAt1":true,"supportsVariableType":true,"supportsVariablePaging":true,"supportsRunInTerminalRequest":false}))?;
        self.pending.insert(seq, Pending::Initialize);
        self.protocol.deadlines.insert(
            seq,
            (
                Instant::now() + Duration::from_secs(if reverse { 86_400 } else { 20 }),
                "initialize".into(),
            ),
        );
        self.client = Some(client);
        self.state = DebugState::Starting;
        Ok(())
    }
    pub fn local_source_path(&self, path: &str) -> PathBuf {
        let Some(spec) = &self.config.type_options else {
            return path.into();
        };
        let normalized = path.replace('\\', "/");
        spec.path_mappings
            .iter()
            .filter_map(|(local, prefix)| {
                let prefix = prefix.replace('\\', "/");
                let prefix = prefix.trim_end_matches('/');
                let windows = prefix.as_bytes().get(1) == Some(&b':');
                let matches = if windows {
                    normalized
                        .to_ascii_lowercase()
                        .starts_with(&prefix.to_ascii_lowercase())
                } else {
                    normalized.starts_with(prefix)
                };
                if !matches {
                    return None;
                }
                let suffix = &normalized[prefix.len()..];
                if !suffix.is_empty() && !suffix.starts_with('/') {
                    return None;
                }
                Some((
                    prefix.len(),
                    PathBuf::from(local).join(suffix.trim_start_matches('/')),
                ))
            })
            .max_by_key(|(length, _)| *length)
            .map(|(_, path)| path)
            .unwrap_or_else(|| path.into())
    }
    pub(super) fn protocol_response(
        &mut self,
        command: &str,
        epoch: Option<u64>,
        key: Option<i64>,
        body: Value,
    ) {
        if epoch.is_some_and(|epoch| epoch != self.stop_epoch) {
            return;
        }
        match command {
            "modules" => {
                if let Some(modules) = body["modules"].as_array() {
                    for module in modules
                        .iter()
                        .take(1024)
                        .filter(|module| module.to_string().len() <= 65_536)
                    {
                        self.protocol
                            .modules
                            .insert(module["id"].to_string(), module.clone());
                    }
                }
            }
            "loadedSources" => {
                self.protocol.sources = body["sources"]
                    .as_array()
                    .map(|sources| {
                        sources
                            .iter()
                            .take(1024)
                            .filter(|source| source.to_string().len() <= 65_536)
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default()
            }
            "source" => {
                if let Some(reference) = key {
                    self.protocol.source_contents.insert(
                        reference,
                        (
                            body["mimeType"].as_str().unwrap_or("text/plain").into(),
                            body["content"].as_str().unwrap_or("").into(),
                        ),
                    );
                    while self.protocol.source_contents.len() > 32
                        || self
                            .protocol
                            .source_contents
                            .values()
                            .map(|(_, content)| content.len())
                            .sum::<usize>()
                            > 16 * 1024 * 1024
                    {
                        if let Some(key) = self
                            .protocol
                            .source_contents
                            .keys()
                            .copied()
                            .find(|key| Some(*key) != self.protocol.source_open)
                        {
                            self.protocol.source_contents.remove(&key);
                        } else {
                            break;
                        }
                    }
                }
            }
            "pydevdAuthorize"
                if !self.protocol.negotiated || matches!(self.state, DebugState::Failed(_)) =>
            {
                self.pending.clear();
                self.protocol.deadlines.clear();
                self.initialized = false;
                self.config_done = false;
                self.protocol.negotiated = false;
                self.state = DebugState::Starting;
                self.request("initialize",json!({"clientID":"rustpy","adapterID":"pydevd","pathFormat":"path","linesStartAt1":true,"columnsStartAt1":true,"supportsVariableType":true,"supportsRunInTerminalRequest":false}),Pending::Initialize);
            }
            "setExpression" => {
                self.variables.clear();
                self.request_scopes();
                self.refresh_watches();
            }
            "restartFrame" => {
                self.end_suspension();
                self.state = DebugState::Running;
            }
            _ => {}
        }
        self.protocol.custom_results.insert(command.into(), body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn adapter() -> (std::net::TcpListener, DebugSession) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let session = DebugSession::attach_adapter(
            "127.0.0.1",
            port,
            Default::default(),
            Default::default(),
            "fixture",
        )
        .unwrap();
        (listener, session)
    }
    #[test]
    fn module_process_source_and_capability_events_update_real_session_state() {
        let (_listener, mut session) = adapter();
        session
            .protocol
            .negotiate(json!({"supportsModulesRequest":true}));
        session.handle_event(
            "module",
            json!({"reason":"new","module":{"id":"m","name":"module","path":"/remote/m.py"}}),
        );
        session.handle_event("process", json!({"name":"Python","systemProcessId":12}));
        session.handle_event(
            "loadedSource",
            json!({"reason":"new","source":{"name":"generated","sourceReference":7}}),
        );
        session.handle_event(
            "capabilities",
            json!({"capabilities":{"supportsSetExpression":true}}),
        );
        assert_eq!(session.protocol.modules.len(), 1);
        assert_eq!(
            session.protocol.process.as_ref().unwrap()["systemProcessId"],
            12
        );
        assert_eq!(session.protocol.sources.len(), 1);
        assert!(session.protocol.supports("supportsSetExpression"));
        session.handle_event("module", json!({"reason":"removed","module":{"id":"m"}}));
        assert!(session.protocol.modules.is_empty());
    }
    #[test]
    fn source_maps_use_longest_component_prefix_and_support_windows_remote_names() {
        let (_listener, mut session) = adapter();
        use rustpy_project::configuration_types::{ConfigurationSpec, ConfigurationType};
        let mut spec = ConfigurationSpec::new(ConfigurationType::PythonDebugServer);
        spec.path_mappings.insert("/local".into(), "/remote".into());
        spec.path_mappings
            .insert("/specific".into(), "/remote/pkg".into());
        spec.path_mappings
            .insert("/windows".into(), "C:\\Remote".into());
        session.config.type_options = Some(spec);
        assert_eq!(
            session.local_source_path("/remote/pkg/a.py"),
            PathBuf::from("/specific/a.py")
        );
        assert_eq!(
            session.local_source_path("/remotely/a.py"),
            PathBuf::from("/remotely/a.py")
        );
        assert_eq!(
            session.local_source_path("c:\\remote\\a.py"),
            PathBuf::from("/windows/a.py")
        );
    }
    #[test]
    fn obsolete_expression_responses_cannot_refresh_current_suspension_and_optional_requests_are_gated(
    ) {
        let (_listener, mut session) = adapter();
        session.stop_epoch = 5;
        session.protocol_response("setExpression", Some(4), None, json!({"value":"stale"}));
        assert!(!session
            .protocol
            .custom_results
            .contains_key("setExpression"));
        let before = session.pending.len();
        session.request_modules();
        assert_eq!(session.pending.len(), before);
        assert!(session
            .take_errors()
            .iter()
            .any(|error| error.contains("modules")));
        session.protocol_response(
            "source",
            None,
            Some(8),
            json!({"content":"print('dynamic')","mimeType":"text/x-python"}),
        );
        assert_eq!(session.protocol.source_contents[&8].1, "print('dynamic')");
    }
}
