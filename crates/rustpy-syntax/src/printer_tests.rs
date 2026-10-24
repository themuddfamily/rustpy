use super::*;
fn range(a: u32, b: u32) -> TextRange {
    TextRange::new(a.into(), b.into())
}
fn statements(root: &SyntaxNode) -> Vec<SyntaxNode> {
    root.children()
        .filter(|n| Stmt::can_cast(n.kind()))
        .collect()
}
#[test]
fn unchanged_round_trip_keeps_comments_crlf_unicode_and_error_nodes() {
    for source in [
        "# π🙂\r\nx  = (1,  # item\r\n 2)\r\n",
        "def broken(\n# unfinished π🙂\n",
        "x=1; y=2 # inline",
    ] {
        let root = crate::parse(source).syntax_node();
        assert_eq!(print(&root), source);
        assert_eq!(Rewrite::new(root).unwrap().finish().unwrap(), source);
    }
}
#[test]
fn token_and_node_replacement_keep_all_untouched_trivia() {
    let source = "# header π🙂\r\nx  = value  # keep\r\ny=3\r\n";
    let root = crate::parse(source).syntax_node();
    let ident = root
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.text() == "x")
        .unwrap();
    let value = root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::NAME_EXPR && print(n) == "value")
        .unwrap();
    let replacement_root = crate::parse_expression("λ + 1").syntax_node();
    let replacement = replacement_root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::BINARY_EXPR)
        .unwrap();
    let mut rewrite = Rewrite::new(root).unwrap();
    rewrite.replace_token(&ident, "renamed").unwrap();
    rewrite.replace_node(&value, &replacement).unwrap();
    assert_eq!(
        rewrite.finish().unwrap(),
        "# header π🙂\r\nrenamed  = λ + 1  # keep\r\ny=3\r\n"
    );
}
#[test]
fn sibling_move_attaches_same_indent_comments_not_headers_or_blank_lines() {
    let source="#!/usr/bin/python\n# coding: utf-8\n\n# α documentation\na = 1 # inline\n\n# β documentation\nb = 2\n";
    let root = crate::parse(source).syntax_node();
    let nodes = statements(&root);
    let mut rewrite = Rewrite::new(root).unwrap();
    rewrite
        .move_before(&nodes[1], &nodes[0], TriviaAttachment::LeadingComments)
        .unwrap();
    let output = rewrite.finish().unwrap();
    assert_eq!(output,"#!/usr/bin/python\n# coding: utf-8\n\n# β documentation\nb = 2\n# α documentation\na = 1 # inline\n\n");
    assert!(crate::parse(&output).ok());
}
#[test]
fn coding_cookie_on_second_line_never_moves_with_first_statement() {
    let source = "#!/usr/bin/python\n# coding: utf-8\na=1\nb=2\n";
    let root = crate::parse(source).syntax_node();
    let nodes = statements(&root);
    let r = attached_range(source, &nodes[0], TriviaAttachment::LeadingComments).unwrap();
    assert_eq!(
        &source[usize::from(r.start())..usize::from(r.end())],
        "a=1\n"
    );
}
#[test]
fn attachment_keeps_decorators_and_comments_inside_function() {
    let source="def outer():\n    # documentation\n    @decorate\n    def helper():\n        # body\n        return 1 # inline\n    # belongs to return\n    return helper()\n";
    let root = crate::parse(source).syntax_node();
    let helper = root
        .descendants()
        .filter_map(crate::ast::FuncDef::cast)
        .find(|f| f.name_text() == "helper")
        .unwrap();
    let r = attached_range(source, helper.syntax(), TriviaAttachment::LeadingComments).unwrap();
    assert_eq!(&source[usize::from(r.start())..usize::from(r.end())],"    # documentation\n    @decorate\n    def helper():\n        # body\n        return 1 # inline\n");
}
#[test]
fn node_only_does_not_attach_outside_comments() {
    let source = "# retain\na = 1 # outside\nb=2\n";
    let root = crate::parse(source).syntax_node();
    let node = root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::NAME_EXPR && print(n) == "a")
        .unwrap();
    assert_eq!(
        attached_range(source, &node, TriviaAttachment::NodeOnly).unwrap(),
        node.text_range()
    );
    assert_eq!(
        attached_range(source, &node, TriviaAttachment::LeadingComments),
        Err(RewriteError::NotStandaloneStatement)
    );
}
#[test]
fn failed_operation_is_atomic_rejecting_foreign_tree_bad_token_and_conflict() {
    let root = crate::parse("x = 1\n").syntax_node();
    let token = root.first_token().unwrap();
    let foreign = crate::parse("x = 1\n").syntax_node();
    let mut rewrite = Rewrite::new(root.clone()).unwrap();
    assert_eq!(
        rewrite.replace_node(&foreign, &root),
        Err(RewriteError::ForeignElement)
    );
    assert_eq!(
        rewrite.replace_token(&token, "two words"),
        Err(RewriteError::InvalidToken)
    );
    rewrite.replace_token(&token, "π").unwrap();
    assert!(matches!(
        rewrite.replace_range(range(0, 3), "bad".into()),
        Err(RewriteError::Conflict(_))
    ));
    assert_eq!(rewrite.finish().unwrap(), "π = 1\n");
}
#[test]
fn unicode_budget_duplicate_and_same_offset_insertion_contract() {
    assert!(matches!(
        apply_text_edits("é", &[(range(1, 2), "a".into())]),
        Err(RewriteError::InvalidRange(_))
    ));
    assert!(matches!(
        apply_text_edits(
            "abc",
            &[(range(0, 2), "a".into()), (range(1, 3), "b".into())]
        ),
        Err(RewriteError::Conflict(_))
    ));
    assert_eq!(
        apply_text_edits(
            "abc",
            &[
                (range(1, 1), "X".into()),
                (range(1, 2), "Y".into()),
                (range(1, 1), "X".into())
            ]
        )
        .unwrap(),
        "aXYc"
    );
    assert_eq!(
        apply_text_edits(
            "abc",
            &[(range(1, 1), "X".into()), (range(1, 1), "Y".into())]
        )
        .unwrap(),
        "aYXbc"
    );
    assert_eq!(
        apply_text_edits("", &vec![(range(0, 0), String::new()); MAX_EDITS + 1]),
        Err(RewriteError::Budget)
    );
}
#[test]
fn semicolon_attachment_and_unterminated_moved_line_refuse() {
    let root = crate::parse("a=1; b=2\n").syntax_node();
    let node = statements(&root)[0].clone();
    assert_eq!(
        attached_range("a=1; b=2\n", &node, TriviaAttachment::LeadingComments),
        Err(RewriteError::NotStandaloneStatement)
    );
    let root = crate::parse("a=1\nb=2").syntax_node();
    let nodes = statements(&root);
    let mut rewrite = Rewrite::new(root).unwrap();
    assert_eq!(
        rewrite.move_before(&nodes[1], &nodes[0], TriviaAttachment::StatementLines),
        Err(RewriteError::MissingLineBreak)
    );
}
#[test]
fn relocate_group_is_atomic_and_does_not_swallow_invalid_destination() {
    let source = "# first\na=1\nb=2\n";
    let root = crate::parse(source).syntax_node();
    let nodes = statements(&root);
    let mut rewrite = Rewrite::new(root).unwrap();
    assert!(matches!(
        rewrite.relocate_statement(
            &nodes[0],
            999.into(),
            TriviaAttachment::LeadingComments,
            String::new(),
            str::to_string
        ),
        Err(RewriteError::InvalidRange(_))
    ));
    assert!(rewrite.edits().is_empty());
    assert_eq!(rewrite.finish().unwrap(), source);
}

#[test]
fn statement_moves_keep_crlf_and_cr_exact_and_have_no_final_newline_guess() {
    for newline in ["\r\n", "\r"] {
        let source = format!("# first{newline}a=1{newline}# second{newline}b=2{newline}");
        let root = crate::parse(&source).syntax_node();
        let nodes = statements(&root);
        let mut rewrite = Rewrite::new(root).unwrap();
        rewrite
            .move_before(&nodes[1], &nodes[0], TriviaAttachment::LeadingComments)
            .unwrap();
        assert_eq!(
            rewrite.finish().unwrap(),
            format!("# second{newline}b=2{newline}# first{newline}a=1{newline}")
        );
    }
}

#[test]
fn mutable_root_alias_cannot_invalidate_a_rewrite_snapshot() {
    let mutable = crate::parse("x=1\n").syntax_node().clone_for_update();
    let alias = mutable.clone();
    assert!(alias.is_mutable());
    assert!(matches!(
        Rewrite::new(mutable),
        Err(RewriteError::MutableRoot)
    ));
}
#[test]
fn trailing_body_comment_stays_with_function_but_next_sibling_docs_stay() {
    let source="def outer():\n    def helper():\n        return 1\n\n        # trailing body π🙂\n    # next sibling docs\n    return helper()\n";
    let root = crate::parse(source).syntax_node();
    let helper = root
        .descendants()
        .filter_map(crate::ast::FuncDef::cast)
        .find(|f| f.name_text() == "helper")
        .unwrap();
    let range = attached_range(source, helper.syntax(), TriviaAttachment::LeadingComments).unwrap();
    assert_eq!(
        &source[usize::from(range.start())..usize::from(range.end())],
        "    def helper():\n        return 1\n\n        # trailing body π🙂\n"
    );
}
#[test]
fn formatter_and_region_boundaries_do_not_attach_to_one_statement() {
    for boundary in [
        "# fmt: off",
        "#fmt:off",
        "# notes; fmt: off",
        "# note # fmt: on",
        "# yapf: disable",
        "# region helpers",
        "# <editor-fold desc='helpers'>",
    ] {
        let source = format!("{boundary}\n# ordinary documentation\na=1\nb=2\n");
        let root = crate::parse(&source).syntax_node();
        let node = statements(&root)[0].clone();
        let range = attached_range(&source, &node, TriviaAttachment::LeadingComments).unwrap();
        assert_eq!(
            &source[usize::from(range.start())..usize::from(range.end())],
            "# ordinary documentation\na=1\n"
        );
    }
}
