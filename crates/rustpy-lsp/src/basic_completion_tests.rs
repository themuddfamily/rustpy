use super::*;

fn basic_list(client: &mut Client, uri: &Uri, position: Position) -> Vec<CompletionItem> {
    let result: Option<CompletionResponse> =
        client.request::<request::Completion>(CompletionParams {
            text_document_position: tdp(uri, position),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        });
    match result.expect("completion response") {
        CompletionResponse::List(list) => list.items,
        CompletionResponse::Array(items) => items,
    }
}

#[test]
fn basic_annotation_completion_keeps_import_edit_metadata() {
    let mut client = Client::start(&[("m.py", "value: Opt\n")]);
    let uri = client.open("m.py");
    let items = basic_list(&mut client, &uri, pos(0, 10));
    let item = items
        .iter()
        .find(|item| item.label == "Optional")
        .expect("typing name");
    assert!(item
        .additional_text_edits
        .as_ref()
        .is_some_and(|edits| edits
            .iter()
            .any(|edit| edit.new_text.contains("import Optional"))));
    let Some(CompletionTextEdit::Edit(edit)) = &item.text_edit else {
        panic!("replacement edit")
    };
    assert_eq!(edit.range, Range::new(pos(0, 7), pos(0, 10)));
    assert!(
        item.data.is_some(),
        "existing resolve cache metadata retained"
    );
}

#[test]
fn basic_dictionary_key_completion_uses_utf16_string_segment() {
    let mut client = Client::start(&[("m.py", "values = {'π-name': 1}\nvalues['π-']\n")]);
    let uri = client.open("m.py");
    let items = basic_list(&mut client, &uri, pos(1, 10));
    let item = items
        .iter()
        .find(|item| item.label == "π-name")
        .expect("literal key");
    let Some(CompletionTextEdit::Edit(edit)) = &item.text_edit else {
        panic!("replacement edit")
    };
    assert_eq!(edit.range, Range::new(pos(1, 8), pos(1, 10)));
    assert_eq!(edit.new_text, "π-name");
}
