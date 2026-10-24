//! Negotiated DAP features and bounded diagnostic/source state. Optional
//! requests are offered only when the connected adapter advertises them.
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    time::Instant,
};
#[derive(Default)]
pub struct ProtocolState {
    pub capabilities: Value,
    pub negotiated: bool,
    pub modules: BTreeMap<String, Value>,
    pub sources: Vec<Value>,
    pub source_contents: BTreeMap<i64, (String, String)>,
    pub process: Option<Value>,
    pub custom_results: BTreeMap<String, Value>,
    pub frame_sources: HashMap<i64, Value>,
    pub function_breakpoints: Vec<Value>,
    pub data_breakpoints: Vec<Value>,
    pub logs: VecDeque<String>,
    pub logging: bool,
    pub variable_presentation: Value,
    pub deadlines: HashMap<u64, (Instant, String)>,
    pub endpoint: Option<(String, u16, String)>,
    pub source_open: Option<i64>,
    pub expression: String,
    pub expression_value: String,
    pub function_name: String,
    pub data_expression: String,
    pub data_reference: i64,
    pub authorization_token: String,
    pub properties_text: String,
    pub attach_options_text: String,
    pub source_map_path: String,
    pub source_maps_text: String,
}
impl ProtocolState {
    pub fn supports(&self, name: &str) -> bool {
        self.capabilities[name].as_bool().unwrap_or(false)
    }
    pub fn negotiate(&mut self, capabilities: Value) {
        self.capabilities = capabilities;
        self.negotiated = true;
    }
    pub fn update_capabilities(&mut self, capabilities: &Value) {
        if !self.capabilities.is_object() {
            self.capabilities = json!({});
        }
        if let Some(fields) = capabilities.as_object() {
            for (key, value) in fields {
                self.capabilities[key] = value.clone();
            }
        }
    }
    pub fn log(&mut self, direction: &str, command: &str, value: &Value) {
        if !self.logging {
            return;
        }
        // Values may contain credentials, source and runtime values. Diagnostics
        // retain the command and bounded structural summary, never those bodies.
        let summary = if let Some(fields) = value.as_object() {
            fields
                .keys()
                .take(24)
                .map(|key| key.chars().take(80).collect::<String>())
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            value.kind_name().to_string()
        };
        self.logs.push_back(format!(
            "{direction} {} [{summary}]",
            command.chars().take(80).collect::<String>()
        ));
        while self.logs.len() > 1000 {
            self.logs.pop_front();
        }
    }
}
trait KindName {
    fn kind_name(&self) -> &'static str;
}
impl KindName for Value {
    fn kind_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_capabilities_are_false_and_updates_preserve_other_negotiated_features() {
        let mut state = ProtocolState::default();
        assert!(!state.supports("supportsSetExpression"));
        state.negotiate(json!({"supportsSetExpression":true,"supportsRestartFrame":false}));
        state.update_capabilities(&json!({"supportsRestartFrame":true}));
        assert!(state.supports("supportsSetExpression") && state.supports("supportsRestartFrame"));
    }
    #[test]
    fn protocol_log_excludes_credentials_values_and_source_but_retains_structural_evidence() {
        let mut state = ProtocolState {
            logging: true,
            ..Default::default()
        };
        for _ in 0..1005 {
            state.log(
                "→",
                "pydevdAuthorize",
                &json!({"debugServerAccessToken":"private-token","expression":"secret source"}),
            );
        }
        assert_eq!(state.logs.len(), 1000);
        let text = state.logs.iter().cloned().collect::<String>();
        assert!(text.contains("pydevdAuthorize") && text.contains("debugServerAccessToken"));
        assert!(!text.contains("private-token") && !text.contains("secret source"));
    }
}
