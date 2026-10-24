use rustpy_syntax::{
    exports::{self, Exports, MAX_NAMES},
    parse,
};
fn evaluate(source: &str) -> Option<Exports> {
    exports::evaluate(&parse(source).syntax_node())
}
#[test]
fn literals_constants_and_module_mutations_share_exact_authority() {
    let source="NAME='π'+'🙂'\nBASE=['public',NAME]\n__all__:list[str]=BASE+['tail']\n__all__ += ['added']\n__all__.append('end')\n__all__.extend(['extra'])\n__all__.remove('public')\n";
    assert_eq!(
        evaluate(source),
        Some(Exports {
            names: vec!["π🙂", "tail", "added", "end", "extra"]
                .into_iter()
                .map(String::from)
                .collect(),
            exact: true
        })
    );
}
#[test]
fn alias_mutation_and_python_remove_first_occurrence_are_preserved() {
    assert_eq!(
        evaluate("N=['a','a']\n__all__=N\nN.append('π')\n__all__.remove('a')\n"),
        Some(Exports {
            names: vec!["a".into(), "π".into()],
            exact: true
        })
    );
    let missing = evaluate("__all__=['a']\n__all__.remove('missing')\n").unwrap();
    assert_eq!(missing.names, vec!["a"]);
    assert!(!missing.exact);
}
#[test]
fn unknown_expressions_are_partial_without_descendant_strings() {
    assert_eq!(
        evaluate("__all__=['known'] + core.__all__\n"),
        Some(Exports {
            names: vec!["known".into()],
            exact: false
        })
    );
    assert_eq!(
        evaluate("__all__=[n for n in ('invented',)]\n"),
        Some(Exports::default())
    );
    assert!(
        evaluate("class C:\n    __all__=['local']\ndef f():\n    __all__=['local']\n").is_none()
    );
    assert!(!evaluate("if flag:\n    __all__=['maybe']\n").unwrap().exact);
}
#[test]
fn imported_names_invalidate_constant_authority() {
    let got = evaluate("N=['stale']\nfrom other import N\n__all__=N\n").unwrap();
    assert!(got.names.is_empty());
    assert!(!got.exact);
    let got = evaluate("__all__=['x']\n__all__.extend(value=['fake'])\n").unwrap();
    assert_eq!(got.names, vec!["x"]);
    assert!(!got.exact);
}
#[test]
fn iterative_deep_concat_preserves_order_and_budget_is_nonexact() {
    let source = format!(
        "__all__={}\n",
        (0..11000)
            .map(|i| format!("['n{i}']"))
            .collect::<Vec<_>>()
            .join("+")
    );
    let got = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || evaluate(&source).unwrap())
        .unwrap()
        .join()
        .unwrap();
    assert!(got.exact);
    assert_eq!(got.names.len(), 11000);
    assert_eq!(got.names[10999], "n10999");
    let source = format!(
        "__all__=[{}]\n",
        (0..MAX_NAMES + 1)
            .map(|_| "'x'")
            .collect::<Vec<_>>()
            .join(",")
    );
    let got = evaluate(&source).unwrap();
    assert_eq!(got.names.len(), MAX_NAMES);
    assert!(!got.exact);
}

#[test]
fn sequence_kind_and_tuple_rebinding_never_fabricate_exact_exports() {
    assert!(!evaluate("__all__=['a']+('b',)\n").unwrap().exact);
    assert!(evaluate("__all__=('a',)+('b',)\n").unwrap().exact);
    assert_eq!(
        evaluate("BASE=('a',)\n__all__=BASE\nBASE+=('b',)\n")
            .unwrap()
            .names,
        vec!["a"]
    );
    assert_eq!(
        evaluate("__all__=('a',)\n__all__+=('b',)\n").unwrap(),
        Exports {
            names: vec!["a".into(), "b".into()],
            exact: true
        }
    );
    assert!(
        !evaluate("__all__=('a',)\n__all__.append('b')\n")
            .unwrap()
            .exact
    );
    assert!(!evaluate("__all__=('a',)\n__all__+=['b']\n").unwrap().exact);
    assert!(evaluate("__all__=['a']\n__all__+=('b',)\n").unwrap().exact);
}
#[test]
fn uncertainty_and_unsupported_writes_propagate_through_aliases() {
    for source in [
        "if flag:\n    NAME='hidden'\n__all__=[NAME]\n",
        "__all__=['a']\ndel __all__[0]\n",
        "BASE=['a']\n__all__=BASE\nBASE[0]='b'\n",
        "__all__=[b'bad']\n",
        "__all__=['unterminated]\n",
    ] {
        assert!(!evaluate(source).unwrap().exact, "{source}");
    }
    assert!(evaluate("__all__=[r'π',u'🙂']\n").unwrap().exact);
}
#[test]
fn augmented_growth_across_many_bindings_respects_total_budget() {
    let mut source = format!("S='{}'\n", "x".repeat(4096));
    for i in 0..16 {
        source.push_str(&format!("L{i}=[]\n"));
        for _ in 0..20 {
            source.push_str(&format!("L{i}+=[S]\n"));
        }
    }
    source.push_str("__all__=L15\n");
    let got = evaluate(&source).unwrap();
    assert!(!got.exact);
    assert!(got.names.iter().map(String::len).sum::<usize>() <= exports::MAX_TEXT);
}
