use super::*;
const VALUES: &str = include_str!("../backend_values.py");
impl DebugSession {
    pub fn inspect_value(&mut self, expression: &str, kind: &str) {
        if !self.is_stopped() {
            return;
        }
        let options = json!({"offset":self.backend.offset,"rows":if self.backend.rows==0{100}else{self.backend.rows},"columns":if self.backend.columns==0{100}else{self.backend.columns},"slice":self.backend.slice,"col_start":self.backend.scientific_column_offset});
        let wire=format!("(lambda _rp, _value: (__import__('builtins').exec({}, _rp), _rp['render'](_value, {}, {}))[1])({{}}, ({}))",serde_json::to_string(&format!("{}\n{}\n{}", crate::scientific::TABLE_SOURCE,crate::scientific::PLOT_SOURCE,VALUES)).unwrap(),serde_json::to_string(kind).unwrap(),serde_json::to_string(&options.to_string()).map(|json|format!("__import__('json').loads({json})")).unwrap(),expression);
        self.backend.pending_view = true;
        if kind != "plots" && kind != "plots_disabled" {
            self.backend.renderer = kind.into();
        }
        self.request("evaluate",json!({"expression":wire,"context":"repl","frameId":self.current_frame().map(|frame|frame.id)}),Pending::BackendView {expression:expression.into(),kind:kind.into(),epoch:self.stop_epoch});
    }
    pub fn inspect_async_tasks(&mut self) {
        if !self.is_stopped() {
            return;
        }
        let wire=format!("(lambda _rp: (__import__('builtins').exec({}, _rp), _rp['async_snapshot']())[1])({{}})",serde_json::to_string(&format!("{}\n{}\n{}", crate::scientific::TABLE_SOURCE,crate::scientific::PLOT_SOURCE,VALUES)).unwrap());
        self.request("evaluate",json!({"expression":wire,"context":"repl","frameId":self.current_frame().map(|frame|frame.id)}),Pending::BackendView {expression:"asyncio tasks".into(),kind:"async".into(),epoch:self.stop_epoch});
    }
    pub fn mark_object(&mut self, expression: &str, label: &str) {
        if !self.is_stopped() {
            return;
        }
        let code=format!("(lambda _b: (_b.__dict__.setdefault('_rustpy_debug_marks', {{}}).__setitem__({}, ({}))))(__import__('builtins'))",serde_json::to_string(label).unwrap(),expression);
        self.evaluate_repl(&code);
        self.backend.labels.insert(label.into(), expression.into());
    }
    pub fn request_variables_page(&mut self, reference: i64, start: usize) {
        if !self.is_stopped() || reference == 0 || self.vars_in_flight.contains(&reference) {
            return;
        }
        self.vars_in_flight.insert(reference);
        let count = self.options.backend.max_children.clamp(1, 1000);
        self.request(
            "variables",
            json!({"variablesReference":reference,"start":start,"count":count}),
            Pending::VariablesPage {
                reference,
                start,
                epoch: self.stop_epoch,
            },
        );
    }
    pub(super) fn backend_variables_response(
        &mut self,
        reference: i64,
        start: usize,
        epoch: u64,
        body: Value,
    ) {
        if epoch != self.stop_epoch {
            return;
        }
        self.vars_in_flight.remove(&reference);
        let rows = body["variables"]
            .as_array()
            .map(|rows| rows.iter().map(parse_variable).collect::<Vec<_>>())
            .unwrap_or_default();
        let count = rows.len();
        let limit = self.options.backend.max_children.clamp(1, 1000);
        self.backend.page_counts.insert(reference, start + count);
        if count >= limit {
            self.backend.page_more.insert(reference);
        } else {
            self.backend.page_more.remove(&reference);
        }
        if start == 0 {
            self.variables.insert(reference, rows);
        } else {
            self.variables.entry(reference).or_default().extend(rows);
        }
    }
    pub(super) fn backend_view_response(
        &mut self,
        expression: String,
        kind: String,
        epoch: u64,
        body: Value,
    ) {
        if epoch != self.stop_epoch {
            return;
        }
        self.backend.pending_view = false;
        let text = body["result"].as_str().unwrap_or("");
        // DAP returns a Python repr of the renderer's string. literal_eval in
        // the target would be another execution; decode only quoted strings.
        let decoded = decode_python_string(text);
        if kind == "table" {
            match serde_json::from_str(&decoded) {
                Ok(table) => {
                    self.backend.data_view = Some(table);
                    self.backend.scientific_revision =
                        self.backend.scientific_revision.wrapping_add(1);
                    self.backend.data_expression = expression;
                }
                Err(error) => self
                    .errors
                    .push(format!("Cannot decode data view: {error}")),
            }
        } else if kind == "plots" || kind == "plots_disabled" {
            if let Ok(plots) = serde_json::from_str::<Vec<Value>>(&decoded) {
                self.backend.scientific_plots.extend(plots);
            }
        } else if kind == "async" {
            self.backend.async_tasks = Some(decoded);
        } else {
            self.backend.value_view = Some((expression, kind, decoded));
        }
    }
    pub(super) fn apply_breakpoint_dependencies(&mut self, hits: &[(PathBuf, usize)]) {
        let mut refresh = HashSet::new();
        for point in &self.options.backend.breakpoints {
            if point
                .metadata
                .depends_on
                .as_ref()
                .is_some_and(|master| hits.contains(master))
            {
                self.backend
                    .dependencies
                    .insert((point.path.clone(), point.line));
                if let Some(bp) = self
                    .breakpoints
                    .get_mut(&point.path)
                    .and_then(|points| points.iter_mut().find(|bp| bp.line == point.line))
                {
                    bp.enabled = true;
                    refresh.insert(point.path.clone());
                }
            }
        }
        for path in refresh {
            self.send_breakpoints_for(&path);
        }
    }
    pub fn copy_stack(&self) -> String {
        self.frames
            .iter()
            .map(|frame| {
                format!(
                    "{} at {}:{}",
                    frame.name,
                    frame
                        .path
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_default(),
                    frame.line
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
fn decode_python_string(text: &str) -> String {
    if let Ok(text) = serde_json::from_str::<String>(text) {
        return text;
    }
    let Some(body) = text
        .strip_prefix('\'')
        .and_then(|text| text.strip_suffix('\''))
    else {
        return text.into();
    };
    let mut output = String::new();
    let mut chars = body.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        match chars.next() {
            Some('n') => output.push('\n'),
            Some('r') => output.push('\r'),
            Some('t') => output.push('\t'),
            Some('\\') => output.push('\\'),
            Some('\'') => output.push('\''),
            Some('"') => output.push('"'),
            Some(kind @ ('u' | 'U' | 'x')) => {
                let count = match kind {
                    'U' => 8,
                    'x' => 2,
                    _ => 4,
                };
                let value = chars.by_ref().take(count).collect::<String>();
                if let Ok(value) = u32::from_str_radix(&value, 16) {
                    if let Some(value) = char::from_u32(value) {
                        output.push(value);
                    }
                }
            }
            Some(other) => {
                output.push('\\');
                output.push(other);
            }
            None => {}
        }
    }
    output
}

impl DebugSession {
    /// debugpy advertises an already connected child with a new DAP connection
    /// to the parent's adapter and its exact subProcessId (clients.py).
    pub fn attach_subprocess(&self, body: &Value) -> anyhow::Result<Self> {
        let (host, port, _) = self
            .protocol
            .endpoint
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Parent adapter endpoint unavailable"))?;
        let advertised_host = body["connect"]["host"].as_str().unwrap_or(host);
        let advertised_port = body["connect"]["port"].as_u64().unwrap_or(*port as u64);
        anyhow::ensure!(
            advertised_host == host && advertised_port == *port as u64,
            "Subprocess advertised a different adapter endpoint"
        );
        let pid = body["subProcessId"]
            .as_u64()
            .filter(|pid| *pid > 0)
            .ok_or_else(|| anyhow::anyhow!("Missing subprocess id"))?;
        let mut child = Self::attach_transport(
            host,
            *port,
            self.options.clone(),
            self.breakpoints.clone(),
            "debugpy",
            false,
            Some(self._port_lease.shared()),
        )?;
        child.config = self.config.clone();
        let mut spec = rustpy_project::configuration_types::ConfigurationSpec::new(
            rustpy_project::configuration_types::ConfigurationType::PythonDebugServer,
        );
        spec.host = host.clone();
        spec.port = *port;
        spec.debug_options = json!({"subProcessId":pid});
        if let Some(parent) = &self.config.type_options {
            spec.path_mappings = parent.path_mappings.clone();
        }
        child.config.type_options = Some(spec);
        child.name = body["name"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Subprocess {pid}"));
        child.config.name = child.name.clone();
        child.exceptions = self.exceptions.clone();
        child.muted = self.muted;
        Ok(child)
    }
}

impl DebugSession {
    /// Temporarily disable debugpy's library filter before the step request.
    pub fn force_step_into(&mut self) {
        if !self.is_stopped() || self.is_javascript_adapter() {
            return;
        }
        if self
            .protocol
            .endpoint
            .as_ref()
            .is_some_and(|(_, _, adapter)| adapter == "pydevd-legacy")
        {
            self.request(
                "rustpyForceStepInto",
                json!({"threadId":self.control_thread()}),
                Pending::Control,
            );
            self.state = DebugState::Running;
            self.end_suspension();
            return;
        }
        let expression="(lambda _db, _b: (_b.__dict__.__setitem__('_rustpy_force_step_filter', _db.get_use_libraries_filter()), _db.set_use_libraries_filter(False)))(__import__('_pydevd_bundle.pydevd_constants', fromlist=['get_global_debugger']).get_global_debugger(), __import__('builtins'))";
        self.request("evaluate",json!({"expression":expression,"context":"repl","frameId":self.current_frame().map(|frame|frame.id)}),Pending::BackendCommand{kind:"force-step".into(),epoch:self.stop_epoch});
    }
    pub fn force_run_to_cursor(&mut self, path: &Path, line: usize) {
        if !self.is_stopped() {
            return;
        }
        self.backend.restore_mute = Some(self.muted);
        self.set_muted(true);
        self.run_to_cursor(path, line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn python_renderer_results_decode_without_target_execution() {
        assert_eq!(
            decode_python_string(r#"'{"rows": [["hello"]]}\n'"#),
            "{\"rows\": [[\"hello\"]]}\n"
        );
        assert_eq!(decode_python_string(r#"'\u03bb\tvalue'"#), "λ\tvalue");
        assert_eq!(decode_python_string("unquoted"), "unquoted");
    }
}
