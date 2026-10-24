use rustpy_grammar::{
    apply_first_fixes,
    policy::{self, Category, Context, Language, Policy},
};

#[test]
fn category_preferences_select_actual_rules_and_fixes() {
    let text = "i think the the answer could of helped alot , today. then we exit.";
    let all = policy::check(text, Context::Prose, Policy::default());
    for category in Category::ALL {
        assert!(all
            .iter()
            .any(|i| policy::rule_category(i.rule) == Some(category)));
        let selected = Policy::from_preferences("en-US", [category.key()]).unwrap();
        let filtered = policy::check(text, Context::Prose, selected);
        assert!(filtered
            .iter()
            .all(|i| policy::rule_category(i.rule) != Some(category)));
        assert_eq!(
            filtered.len(),
            all.iter()
                .filter(|i| policy::rule_category(i.rule) != Some(category))
                .count()
        );
    }
    assert_eq!(
        apply_first_fixes(
            "the the",
            &policy::check("the the", Context::CommitMessage, Policy::default())
        ),
        "the"
    );
}

#[test]
fn language_selection_is_explicit_and_unsupported_preferences_fail() {
    for language in Language::ALL {
        assert_eq!(Language::from_key(language.key()).unwrap(), language);
        assert_eq!(
            policy::check(
                "a apple",
                Context::Prose,
                Policy::from_preferences(language.key(), []).unwrap()
            )[0]
            .replacements,
            ["an"]
        );
    }
    assert!(Policy::from_preferences("fr-FR", []).is_err());
    assert!(Policy::from_preferences("en-US", ["unknown"]).is_err());
}

#[test]
fn all_prose_surfaces_obey_the_same_category_policy() {
    let policy = Policy::from_preferences("en-GB", ["grammar"]).unwrap();
    for (context, source) in [
        (Context::Python, "# the the answer happens alot\n"),
        (Context::Markdown, "the the answer happens alot"),
        (Context::CommitMessage, "the the answer happens alot"),
        (Context::Prose, "the the answer happens alot"),
    ] {
        let issues = policy::check(source, context, policy);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].rule, "common-mistake");
        assert_eq!(&source[issues[0].range.clone()], "alot");
    }
}

#[test]
fn python_checks_actual_docstrings_not_code_strings() {
    let source = "'Module the the value.'\nassigned = '''Code the the value.'''\nclass C:\n    r\"Class the the value.\"\n    def method(self):\n        \"\"\"Return the the value.\"\"\"\n        return 'the the'\ndef later():\n    pass\n    '''Not the the docstring.'''\ndef bytes_doc():\n    b'the the'\ndef formatted_doc():\n    f'the the'\n# the the comment\n";
    let issues = policy::check(source, Context::Python, Policy::default());
    assert_eq!(issues.len(), 4);
    assert!(issues.iter().all(|i| i.rule == "repeated-word"));
    assert_eq!(
        issues
            .iter()
            .map(|i| &source[i.range.clone()])
            .collect::<Vec<_>>(),
        ["the the"; 4]
    );
    let fixed = apply_first_fixes(source, &issues);
    assert!(fixed.contains("assigned = '''Code the the value.'''"));
    assert!(fixed.contains("r\"Class the value.\""));
    assert!(fixed.contains("'''Not the the docstring.'''"));
}

#[test]
fn unicode_ranges_and_concatenated_docstring_tokens_remain_source_byte_ranges() {
    let source = "\"λ🙂 the the\" \" a apple\"\n";
    let issues = policy::check(source, Context::Python, Policy::default());
    assert_eq!(
        issues
            .iter()
            .map(|i| (i.rule, &source[i.range.clone()]))
            .collect::<Vec<_>>(),
        [("repeated-word", "the the"), ("article", "a")]
    );
    assert_eq!(
        apply_first_fixes(source, &issues),
        "\"λ🙂 the\" \" an apple\"\n"
    );
}

#[test]
fn markdown_code_and_python_directives_are_not_checked() {
    let markdown = "the the\n\n```\na apple\n```\n`a apple`\n";
    assert_eq!(
        policy::check(markdown, Context::Markdown, Policy::default()).len(),
        1
    );
    let python = "# noqa: the the\n# x = the the\n# Return the the value.\n";
    let issues = policy::check(python, Context::Python, Policy::default());
    assert_eq!(issues.len(), 1);
    assert_eq!(&python[issues[0].range.clone()], "the the");
}
