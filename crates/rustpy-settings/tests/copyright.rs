use rustpy_settings::copyright::*;
use std::path::Path;
fn vars() -> Variables<'static> {
    Variables {
        year: "2026",
        date: "2026-10-12",
        project: "café",
        user: "$file",
        file: "模块.py",
    }
}
fn apply(source: &str, profile: &CopyrightProfile, path: &str) -> String {
    match plan_update(source, Path::new(path), profile, &vars()).unwrap() {
        Some(edit) => format!(
            "{}{}{}",
            &source[..edit.start],
            edit.replacement,
            &source[edit.end..]
        ),
        None => source.into(),
    }
}
#[test]
fn variables_single_pass_unicode_unknown_and_literal_dollar() {
    assert_eq!(
        expand("$today.year $today.date $project $user $file $$", &vars()).unwrap(),
        "2026 2026-10-12 café $file 模块.py $"
    );
    assert!(expand("$unknown", &vars()).is_err());
}
#[test]
fn shebang_cookie_unicode_body_and_idempotency() {
    let source = "#!/usr/bin/python\n# -*- coding: utf-8 -*-\n\"\"\"Module docs.\"\"\"\nπ = '😀'\n";
    let profile = CopyrightProfile::default();
    let result = apply(source, &profile, "a.py");
    assert!(result.starts_with("#!/usr/bin/python\n# -*- coding: utf-8 -*-\n# ---\n# Copyright"));
    assert!(result.ends_with("\"\"\"Module docs.\"\"\"\nπ = '😀'\n"));
    assert_eq!(apply(&result, &profile, "a.py"), result);
}
#[test]
fn crlf_bom_cookie_and_headerless_shebang_are_preserved() {
    let profile = CopyrightProfile::default();
    let source = "\u{feff}#!/bin/python\r\n# coding=latin-1\r\nx = 1\r\n";
    let result = apply(source, &profile, "a.py");
    assert!(result.starts_with("\u{feff}#!/bin/python\r\n# coding=latin-1\r\n# ---\r\n# Copyright"));
    assert!(!result.replace("\r\n", "").contains('\n'));
    assert_eq!(apply(&result, &profile, "a.py"), result);
    assert!(
        apply("#!/bin/python", &profile, "a.py").starts_with("#!/bin/python\n# ---\n# Copyright")
    );
}
#[test]
fn existing_notice_does_not_erase_directives_or_unrelated_prefix() {
    let source="# lint note\n# pylint: disable=unused-import\n# Copyright 2000 Old\n# All rights reserved.\n# noqa: F401\nimport os\n";
    let result = apply(source, &CopyrightProfile::default(), "a.py");
    assert!(result.starts_with("# lint note\n# pylint: disable=unused-import\n# ---\n# Copyright"));
    assert!(result.ends_with("# noqa: F401\nimport os\n"));
    assert!(!result.contains("2000"));
    assert_eq!(apply(&result, &CopyrightProfile::default(), "a.py"), result);
}
#[test]
fn policy_add_only_update_only_and_multiple_owned_blocks() {
    let mut profile = CopyrightProfile {
        policy: UpdatePolicy::AddMissingOnly,
        ..Default::default()
    };
    let source = "# Copyright original\nx=1\n";
    assert_eq!(apply(source, &profile, "a.py"), source);
    assert!(apply("x=1\n", &profile, "a.py").contains("Copyright"));
    profile.policy = UpdatePolicy::UpdateExistingOnly;
    assert_eq!(apply("x=1\n", &profile, "a.py"), "x=1\n");
    assert!(apply(source, &profile, "a.py").contains("2026"));
    assert!(plan_update(
        "# Copyright A\n\n# Copyright B\nx=1\n",
        Path::new("a.py"),
        &profile,
        &vars()
    )
    .is_err());
}
#[test]
fn styles_wrap_border_and_no_body_scanning() {
    let profile = CopyrightProfile {
        style: CommentStyle::Block,
        width: 18,
        text: "Copyright alpha beta gamma delta".into(),
        ..Default::default()
    };
    for (path, prefix, suffix) in [
        (
            "a.py",
            "# ----------------\n",
            "# ----------------\n\nx=1\n",
        ),
        ("a.js", "/*\n", " */\n\nx=1\n"),
        ("a.xml", "<!--\n", "-->\n\nx=1\n"),
    ] {
        let result = apply("x=1\n", &profile, path);
        assert!(result.starts_with(prefix));
        assert!(result.ends_with(suffix));
        assert_eq!(apply(&result, &profile, path), result);
    }
    let source = "x=1\n# Copyright body comment\n";
    assert!(apply(source, &CopyrightProfile::default(), "a.py").ends_with(source));
    let p = CopyrightProfile {
        border: true,
        ..Default::default()
    };
    let result = apply("x=1\n", &p, "a.py");
    assert_eq!(apply(&result, &p, "a.py"), result);
}
#[test]
fn unsafe_comment_delimiters_and_unsupported_formats_refuse() {
    let separator = CopyrightProfile {
        text: "Copyright license\n----------------\nmore".into(),
        ..Default::default()
    };
    assert!(plan_update("", Path::new("a.py"), &separator, &vars()).is_err());
    let profile = CopyrightProfile {
        text: "Copyright */ unsafe".into(),
        style: CommentStyle::Block,
        ..Default::default()
    };
    assert!(plan_update("", Path::new("a.js"), &profile, &vars()).is_err());
    let profile = CopyrightProfile {
        text: "Copyright -- unsafe".into(),
        ..Default::default()
    };
    assert!(plan_update("", Path::new("a.xml"), &profile, &vars()).is_err());
    assert!(plan_update(
        "{}",
        Path::new("a.json"),
        &CopyrightProfile::default(),
        &vars()
    )
    .is_err());
}
#[test]
fn ordered_scopes_exclusions_outside_paths_and_serde_migration() {
    let settings = CopyrightProfilesSettings {
        profiles: vec![
            CopyrightProfile::default(),
            CopyrightProfile {
                name: "Tests".into(),
                ..Default::default()
            },
        ],
        default_profile: Some("Default".into()),
        scopes: vec![
            CopyrightScope {
                name: "generated".into(),
                include: "generated/**".into(),
                profile: String::new(),
                exclude: String::new(),
            },
            CopyrightScope {
                name: "tests".into(),
                include: "**/test_?.py".into(),
                exclude: "vendor/**".into(),
                profile: "Tests".into(),
            },
        ],
    };
    settings.validate().unwrap();
    assert!(settings
        .profile_for(Path::new("/p/generated/a.py"), Some(Path::new("/p")))
        .unwrap()
        .is_none());
    assert_eq!(
        settings
            .profile_for(Path::new("/p/test_a.py"), Some(Path::new("/p")))
            .unwrap()
            .unwrap()
            .name,
        "Tests"
    );
    assert_eq!(
        settings
            .profile_for(Path::new("/p/pkg/test_b.py"), Some(Path::new("/p")))
            .unwrap()
            .unwrap()
            .name,
        "Tests"
    );
    assert_eq!(
        settings
            .profile_for(Path::new("/other/a.py"), Some(Path::new("/p")))
            .unwrap()
            .unwrap()
            .name,
        "Default"
    );
    let encoded = serde_json::to_vec(&settings).unwrap();
    assert_eq!(
        serde_json::from_slice::<CopyrightProfilesSettings>(&encoded).unwrap(),
        settings
    );
    let legacy: rustpy_settings::Settings = serde_json::from_str(
        r#"{"copyright":{"enabled":true,"text":"Copyright old","add_to_new_files":true}}"#,
    )
    .unwrap();
    assert!(legacy.copyright_profiles.profiles.is_empty());
    assert_eq!(legacy.copyright.text, "Copyright old");
}

#[test]
fn xml_declaration_stays_first_and_trailing_comments_are_not_owned() {
    let profile = CopyrightProfile::default();
    let xml = "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n<root/>\r\n";
    let updated = apply(xml, &profile, "a.xml");
    assert!(updated.starts_with("\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n<!--"));
    assert!(updated.ends_with("<root/>\r\n"));
    assert_eq!(apply(&updated, &profile, "a.xml"), updated);
    let old =
        "# Copyright 2000 Old\n# Unrelated migration explanation\n# Keep this exact comment\nx=1\n";
    let updated = apply(old, &profile, "a.py");
    assert!(
        updated.ends_with("# Unrelated migration explanation\n# Keep this exact comment\nx=1\n")
    );
    assert_eq!(apply(&updated, &profile, "a.py"), updated);
    let multiline = CopyrightProfile {
        text: "Copyright $today.year\nLicense line one\n\nLicense line two".into(),
        ..Default::default()
    };
    let updated = apply("# Unrelated source note\nx=1\n", &multiline, "a.py");
    assert_eq!(apply(&updated, &multiline, "a.py"), updated);
}
#[test]
fn configured_empty_profile_set_is_invalid_not_legacy_fallback() {
    let settings = CopyrightProfilesSettings {
        default_profile: Some("Deleted".into()),
        ..Default::default()
    };
    assert!(!settings.is_unconfigured());
    assert!(settings.validate().is_err());
    assert!(CopyrightProfilesSettings::default().is_unconfigured());
}

#[test]
fn custom_legacy_notice_with_c_symbol_remains_updatable() {
    let profile = CopyrightProfile::from_legacy("(c) $today.year $user");
    assert_eq!(profile.marker, "(c)");
    let updated = apply("# (c) 2000 old\n# Unrelated note\nx=1\n", &profile, "a.py");
    assert!(updated.contains("# (c) 2026 $file"));
    assert!(updated.ends_with("# Unrelated note\nx=1\n"));
    assert_eq!(apply(&updated, &profile, "a.py"), updated);
}

#[test]
fn notice_title_before_marker_has_bounded_start_for_all_hash_styles() {
    for style in [CommentStyle::Line, CommentStyle::Block] {
        for border in [false, true] {
            let profile = CopyrightProfile {
                text: "Legal notice\nCopyright $today.year\nLicense details".into(),
                style,
                border,
                ..Default::default()
            };
            let source =
                "# Unrelated prefix\n# Copyright 2000 Old\n# Unrelated trailing explanation\nx=1\n";
            let updated = apply(source, &profile, "a.py");
            assert!(updated.starts_with("# Unrelated prefix\n"));
            assert!(updated.ends_with("# Unrelated trailing explanation\nx=1\n"));
            assert_eq!(updated.matches("Legal notice").count(), 1);
            assert_eq!(
                apply(&updated, &profile, "a.py"),
                updated,
                "{style:?} border={border}"
            );
        }
    }
    let unbounded="# ---\n# Keep this unrelated title\n# Copyright 2000 Old\n# Keep this unrelated tail\nx=1\n";
    let updated = apply(unbounded, &CopyrightProfile::default(), "a.py");
    assert!(updated.starts_with("# ---\n# Keep this unrelated title\n"));
    assert!(updated.ends_with("# Keep this unrelated tail\nx=1\n"));
    let profile = CopyrightProfile {
        text: "Legal notice\nCopyright $today.year".into(),
        ..Default::default()
    };
    let updated = apply("const x=1;\n", &profile, "a.js");
    assert_eq!(apply(&updated, &profile, "a.js"), updated);
}
#[test]
fn copyright_before_second_line_cookie_is_refused_without_duplicate_or_moved_cookie() {
    for source in [
        "# Copyright 2000 Old\n# coding: utf-8\nx=1\n",
        "\u{feff}# Copyright 2000 Old\r\n# coding: latin-1\r\nx=1\r\n",
    ] {
        for policy in [
            UpdatePolicy::ReplaceOrAdd,
            UpdatePolicy::AddMissingOnly,
            UpdatePolicy::UpdateExistingOnly,
        ] {
            let profile = CopyrightProfile {
                policy,
                ..Default::default()
            };
            let result = plan_update(source, Path::new("a.py"), &profile, &vars());
            assert!(result.is_err());
            assert_eq!(source.matches("Copyright").count(), 1);
            assert!(source.lines().nth(1).unwrap().starts_with("# coding:"));
        }
    }
}

#[test]
fn delimiter_markers_are_refused_before_any_header_edit() {
    for marker in ["-", "#", "/", "*", "<", "Copyright\n"] {
        let profile = CopyrightProfile {
            marker: marker.into(),
            text: format!("Legal {marker} notice"),
            ..Default::default()
        };
        let source = "x=1\n";
        assert!(plan_update(source, Path::new("a.py"), &profile, &vars()).is_err());
        let settings = CopyrightProfilesSettings {
            profiles: vec![profile],
            default_profile: Some("Default".into()),
            scopes: vec![],
        };
        assert!(settings.validate().is_err());
    }
}
