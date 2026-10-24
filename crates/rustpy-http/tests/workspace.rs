//! Pure source fixtures; no transport, script runtime or external process is invoked.
use rustpy_http::{parse, workspace, HttpRequest};
#[test]
fn directives_scripts_and_protocols_remain_attached_to_captured_blocks() {
    let requests = parse(include_str!("fixtures/workspace.http"));
    assert_eq!(requests.len(), 4);
    let first = &requests[0];
    assert_eq!(first.name.as_deref(), Some("createRecord"));
    assert!(first.no_redirect && first.no_cookie_jar && first.no_log);
    assert_eq!(first.timeout_ms, Some(1200));
    assert_eq!(first.connection_timeout_ms, Some(300));
    assert!(first
        .pre_script
        .as_deref()
        .unwrap()
        .contains("request.variables.set"));
    assert!(first
        .handler
        .as_deref()
        .unwrap()
        .contains("response.headers.valueOf"));
    assert_eq!(first.save_response.as_deref(), Some("record-response.json"));
    assert!(first.overwrite_response);
    assert_eq!(requests[1].method, "GRAPHQL");
    assert_eq!(requests[2].method, "WEBSOCKET");
    assert_eq!(
        requests[3].directives.get("proto").map(String::as_str),
        Some("records.proto")
    );
}
#[test]
fn curl_conversion_preserves_literal_shell_text_without_executing_it() {
    let request = workspace::from_curl("curl -X POST 'https://example.invalid/a?x=1' -H 'X-Token: $(never)' --data-raw '{\"value\":\"a b\"}'").unwrap();
    assert_eq!(request.headers[0].1, "$(never)");
    let roundtrip = parse(&workspace::to_http(&request));
    assert_eq!(roundtrip[0].body, request.body);
    assert!(workspace::from_curl("curl --unsupported https://example.invalid").is_err());
}
#[test]
fn serialization_keeps_request_policies_and_both_scripts() {
    let request = HttpRequest {
        method: "POST".into(),
        url: "https://example.invalid".into(),
        no_cookie_jar: true,
        timeout_ms: Some(55),
        pre_script: Some("client.global.set('a', 'b');".into()),
        handler: Some("client.assert(true);".into()),
        save_response: Some("body.txt".into()),
        overwrite_response: true,
        ..Default::default()
    };
    let parsed = parse(&workspace::to_http(&request));
    assert!(parsed[0].no_cookie_jar);
    assert_eq!(parsed[0].timeout_ms, Some(55));
    assert!(parsed[0].pre_script.is_some() && parsed[0].handler.is_some());
    assert!(parsed[0].overwrite_response);
}
#[test]
fn imports_generate_runnable_requests_instead_of_labels() {
    let api = workspace::openapi(r#"{"openapi":"3.0.0","servers":[{"url":"https://example.invalid"}],"paths":{"/records/{id}":{"get":{"parameters":[{"name":"id","in":"path"},{"name":"expand","in":"query"}]}}}}"#).unwrap();
    assert_eq!(
        api[0].url,
        "https://example.invalid/records/{{id}}?expand={{expand}}"
    );
    let collection = workspace::postman(r#"{"item":[{"name":"folder","item":[{"name":"Get record","request":{"method":"GET","url":{"raw":"https://example.invalid/record"}}}]}]}"#).unwrap();
    assert_eq!(
        parse(&workspace::to_http(&collection[0]))[0].url,
        "https://example.invalid/record"
    );
    assert!(workspace::template("mptr", "https://example.invalid")
        .unwrap()
        .contains("multipart/form-data"));
}
#[test]
fn yaml_openapi_aliases_unicode_and_json_examples_generate_equivalent_requests() {
    let source = r#"openapi: 3.0.3
# Imported as data only: comments, anchors and scalar styles remain supported.
servers:
  - url: https://example.invalid
paths:
  /records/{id}:
    post: &operation
      summary: >-
        Create π
        record
      parameters:
        - name: id
          in: path
        - name: expand
          in: query
      requestBody:
        content:
          application/json:
            example:
              name: "π🙂"
              enabled: true
              optional: null
              count: 3
              "on": "yes"
      responses:
        200:
          description: Created
    put: *operation
"#;
    let requests = workspace::openapi(source).unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[1].method, "PUT");
    for request in requests {
        assert_eq!(request.name.as_deref(), Some("Create π record"));
        assert_eq!(
            request.url,
            "https://example.invalid/records/{{id}}?expand={{expand}}"
        );
        assert_eq!(
            request.headers,
            vec![("Content-Type".to_string(), "application/json".to_string())]
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(request.body.as_deref().unwrap()).unwrap(),
            serde_json::json!({"name":"π🙂","enabled":true,"optional":null,"count":3,"on":"yes"})
        );
        let reparsed = parse(&workspace::to_http(&request));
        assert_eq!(reparsed[0].body, request.body);
        assert_eq!(reparsed[0].url, request.url);
    }
}

#[test]
fn yaml_openapi_retains_parse_failures_and_missing_paths_diagnostics() {
    assert!(workspace::openapi("paths:\n  /records:\n    get: [unterminated\n").is_err());
    assert!(workspace::openapi("paths: {}\n---\npaths: {}\n").is_err());
    assert_eq!(
        workspace::openapi("openapi: 3.1.0\npaths: []\n").unwrap_err(),
        "OpenAPI spec has no paths object"
    );
}

#[test]
fn aws_signature_changes_with_payload_and_retains_session_token() {
    let auth = serde_json::json!({"region":"us-east-1","service":"execute-api","access_key":"fixture","secret_key":"fixture-secret","session_token":"fixture-session"});
    let mut request = HttpRequest {
        method: "POST".into(),
        url: "https://example.invalid/resource?a=2&a=1".into(),
        body: Some("one".into()),
        ..Default::default()
    };
    rustpy_http::auth::sigv4(&mut request, &auth, "20261015T120000Z").unwrap();
    let signature = request
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("authorization"))
        .unwrap()
        .1
        .clone();
    request.binary_body = Some(b"two".to_vec());
    rustpy_http::auth::sigv4(&mut request, &auth, "20261015T120000Z").unwrap();
    assert_ne!(
        signature,
        request
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("authorization"))
            .unwrap()
            .1
    );
    assert!(request.headers.iter().any(|(key, value)| key
        .eq_ignore_ascii_case("x-amz-security-token")
        && value == "fixture-session"));
}
#[test]
fn parallel_global_deltas_preserve_unrelated_completion_and_support_clear() {
    use std::collections::HashMap;
    let baseline = HashMap::from([("existing".into(), "old".into())]);
    let mut current = baseline.clone();
    let mut first = baseline.clone();
    first.insert("first".into(), "one".into());
    rustpy_http::runtime::merge_globals(&mut current, &baseline, first);
    let mut second = baseline.clone();
    second.insert("second".into(), "two".into());
    rustpy_http::runtime::merge_globals(&mut current, &baseline, second);
    assert_eq!(current.get("first").map(String::as_str), Some("one"));
    assert_eq!(current.get("second").map(String::as_str), Some("two"));
    rustpy_http::runtime::merge_globals(&mut current, &baseline, HashMap::new());
    assert!(!current.contains_key("existing"));
    assert!(current.contains_key("first") && current.contains_key("second"));
}
#[test]
fn cancellation_and_live_message_queues_are_shared_and_bounded() {
    let control = rustpy_http::runtime::Control::default();
    let captured = control.clone();
    control.send("message".into());
    assert_eq!(captured.take_outgoing(), vec!["message".to_string()]);
    assert!(control.take_outgoing().is_empty());
    control.send("x".repeat(1024 * 1024 + 1));
    assert!(captured.take_outgoing().is_empty());
    for index in 0..1001 {
        captured.message(index.to_string());
    }
    assert_eq!(control.messages().len(), 1000);
    captured.cancel();
    assert!(control.cancelled());
}
