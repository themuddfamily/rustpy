//! Bounded source-editor announcements. This is a navigable group, not an
//! editable AT text input: unsupported SetValue/selection actions are not advertised.
use super::EditorState;

pub(crate) fn status(state: &EditorState) -> String {
    let caret = state.primary();
    let pos = state.doc.offset_to_position(caret.head());
    let line = state.doc.line_slice(pos.line as usize);
    let start = (pos.column as usize)
        .saturating_sub(80)
        .min(line.len_chars());
    let end = (start + 160).min(line.len_chars());
    let context: String = line.slice(start..end).chars().collect();
    let mut text = format!(
        "Line {}, column {}. {}. {} caret{}. Line context: {}",
        pos.line + 1,
        pos.column + 1,
        if state.doc.is_read_only() {
            "Read only"
        } else {
            "Editable"
        },
        state.carets.len(),
        if state.carets.len() == 1 { "" } else { "s" },
        context
    );
    if caret.sel.anchor != caret.sel.head {
        let anchor = state.doc.offset_to_position(caret.sel.anchor);
        text.push_str(&format!(
            ". Selection from line {}, column {}",
            anchor.line + 1,
            anchor.column + 1
        ));
    }
    if state.column_mode {
        text.push_str(". Column selection mode");
    }
    let virtual_head = state.virtual_caret(0).head;
    if virtual_head.column > pos.column {
        text.push_str(&format!(". Virtual column {}", virtual_head.column + 1));
    }
    text
}

pub(crate) fn editor(resp: &egui::Response, state: &EditorState, focused: bool) {
    let name = state
        .path
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|p| p.to_string_lossy())
        .unwrap_or_else(|| "Untitled".into());
    let name: String = name.chars().take(256).collect();
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Other,
            true,
            format!("Source editor: {name}"),
        )
    });
    resp.ctx.accesskit_node_builder(resp.id, |node| {
        node.set_role(egui::accesskit::Role::Group);
        // While focused, the live region below speaks every caret move; a
        // description changing in step made Orca speak each move twice
        // (description-change of the focus plus the announcement).
        if !focused {
            node.set_description(status(state));
        }
        if state.doc.is_read_only() {
            node.set_read_only();
        }
    });
    if focused {
        crate::a11y::announce(&resp.ctx, resp.id, &status(state));
    }
}
