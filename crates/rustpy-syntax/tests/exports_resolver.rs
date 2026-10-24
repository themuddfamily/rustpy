use rustpy_syntax::{
    ast::AstNode,
    exports::{self, Exports, ResolvedExportValue, SequenceKind},
    parse,
};
fn run(source: &str, kind: SequenceKind) -> Exports {
    exports::evaluate_with(&parse(source).syntax_node(), &mut |expr| match expr
        .syntax()
        .text()
        .to_string()
        .as_str()
    {
        "other.__all__" => Some(ResolvedExportValue::Sequence {
            exports: Exports {
                names: vec!["external".into()],
                exact: true,
            },
            kind,
        }),
        "OTHER_NAME" => Some(ResolvedExportValue::String {
            value: "π🙂".into(),
            exact: true,
        }),
        _ => None,
    })
    .unwrap()
}
#[test]
fn resolved_kind_controls_concat_alias_mutation_and_tuple_rebinding() {
    assert!(!run("__all__=other.__all__ + ('tail',)\n", SequenceKind::List).exact);
    for source in [
        "__all__=['local']\n__all__ += other.__all__\n",
        "__all__=['local']\n__all__.extend(other.__all__)\n",
    ] {
        let value = run(source, SequenceKind::List);
        assert!(value.exact);
        assert_eq!(value.names, ["local", "external"]);
    }
    let value = run(
        "BASE=other.__all__\n__all__=BASE\nBASE.append('tail')\n",
        SequenceKind::List,
    );
    assert!(value.exact);
    assert_eq!(value.names, ["external", "tail"]);
    let value = run(
        "BASE=other.__all__\n__all__=BASE\nBASE += ('tail',)\n",
        SequenceKind::Tuple,
    );
    assert!(value.exact);
    assert_eq!(value.names, ["external"]);
    assert!(
        !run(
            "BASE=other.__all__\n__all__=BASE\nBASE.append('bad')\n",
            SequenceKind::Tuple
        )
        .exact
    );
    let value = run("__all__=[OTHER_NAME]\n", SequenceKind::List);
    assert!(value.exact);
    assert_eq!(value.names, ["π🙂"]);
}
#[test]
fn default_and_mutated_authority_never_invent_external_exactness() {
    assert!(
        !exports::evaluate(&parse("__all__=other.__all__\n").syntax_node())
            .unwrap()
            .exact
    );
    assert!(
        !run(
            "__all__=['old']\nmutate()\n__all__=other.__all__\n",
            SequenceKind::List
        )
        .exact
    );
    assert!(
        !run(
            "BASE=other.__all__\nBASE.append('changed')\n__all__=other.__all__\n",
            SequenceKind::List
        )
        .exact
    );
    // Locally bound unknown values must not fall back to an external resolver.
    assert!(
        !run(
            "OTHER_NAME=unknown\n__all__=[OTHER_NAME]\n",
            SequenceKind::List
        )
        .exact
    );
}
#[test]
fn resolver_budget_and_partial_results_are_bounded() {
    let source = parse("__all__=other.__all__\n");
    let value = exports::evaluate_with(&source.syntax_node(), &mut |_| {
        Some(ResolvedExportValue::Sequence {
            exports: Exports {
                names: vec!["x".into(); exports::MAX_NAMES + 1],
                exact: true,
            },
            kind: SequenceKind::List,
        })
    })
    .unwrap();
    assert!(!value.exact);
    let value = exports::evaluate_with(&source.syntax_node(), &mut |_| {
        Some(ResolvedExportValue::Sequence {
            exports: Exports {
                names: vec!["known".into()],
                exact: false,
            },
            kind: SequenceKind::List,
        })
    })
    .unwrap();
    assert!(!value.exact);
    assert_eq!(value.names, ["known"]);
}

#[test]
fn distinct_external_occurrences_cannot_keep_stale_exact_peer_copies() {
    for mutation in [
        "BASE.append('tail')",
        "BASE += ['tail']",
        "BASE[0]: str = 'tail'",
    ] {
        let value = run(
            &format!("BASE=other.__all__\n__all__=other.__all__\n{mutation}\n"),
            SequenceKind::List,
        );
        assert!(!value.exact, "{mutation}");
    }
    let value = run(
        "BASE=other.__all__\n__all__=BASE\nBASE.append('tail')\n",
        SequenceKind::List,
    );
    assert!(value.exact);
    assert_eq!(value.names, ["external", "tail"]);
}
#[test]
fn named_binding_interface_preserves_scalar_and_sequence_kind() {
    let source = parse("PREFIX='π'\nEXPORTS=('one','two')\n");
    assert_eq!(
        exports::evaluate_binding_with(&source.syntax_node(), "PREFIX", &mut |_| None),
        Some(ResolvedExportValue::String {
            value: "π".into(),
            exact: true
        })
    );
    assert_eq!(
        exports::evaluate_binding_with(&source.syntax_node(), "EXPORTS", &mut |_| None),
        Some(ResolvedExportValue::Sequence {
            exports: Exports {
                names: vec!["one".into(), "two".into()],
                exact: true
            },
            kind: SequenceKind::Tuple
        })
    );
    assert!(
        exports::evaluate_binding_with(&source.syntax_node(), "missing", &mut |_| None).is_none()
    );
}
