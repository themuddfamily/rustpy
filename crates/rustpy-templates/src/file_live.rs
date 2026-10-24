//! Keep file-template Velocity references separate from live `$FIELD$` tokens.
//! Identifier masks survive optional Python formatting; only captured masks are
//! expanded, so dollar-containing Velocity values are never interpreted again.
use crate::{
    file::{FileTemplate, TemplateSet},
    Expansion,
};
use std::collections::BTreeMap;

pub struct PreparedLive {
    pub text: String,
    masks: Vec<(String, String)>,
    prefix: String,
}
impl PreparedLive {
    pub fn render(
        set: &TemplateSet,
        template: &FileTemplate,
        vars: &BTreeMap<String, String>,
    ) -> Result<Self, crate::file::TooLarge> {
        let mut prefix = "__rustpy_live_".to_string();
        while template.body.contains(&prefix)
            || set.includes.iter().any(|(t, _)| t.body.contains(&prefix))
            || vars.values().any(|v| v.contains(&prefix))
        {
            prefix.push('_');
        }
        let mut masks = Vec::new();
        let mut cloned = set.clone();
        for (include, _) in &mut cloned.includes {
            include.body = protect(&include.body, &prefix, &mut masks)?;
        }
        let mut template = template.clone();
        template.body = protect(&template.body, &prefix, &mut masks)?;
        let text = cloned.try_render(&template, vars)?;
        Ok(Self {
            text,
            masks,
            prefix,
        })
    }
    /// Make an end-of-statement END stop legal Python while formatting. END
    /// inside literals/comments or expressions keeps its ordinary captured mask.
    /// Only uniquely owned comment anchors are removed after formatting.
    pub fn format_python<E: std::fmt::Display>(
        &self,
        text: &str,
        format: impl FnOnce(&str) -> Result<String, E>,
    ) -> Result<String, String> {
        use rustpy_syntax::SyntaxKind;
        let (tokens, _) = rustpy_syntax::tokenize(text);
        let mut anchors = Vec::new();
        for (mask, name) in &self.masks {
            if name != "END" {
                continue;
            }
            for (start, _) in text.match_indices(mask) {
                let end = start + mask.len();
                let line_start = text[..start].rfind('\n').map_or(0, |at| at + 1);
                let line_end = text[end..].find('\n').map_or(text.len(), |at| end + at);
                if !text[end..line_end].trim().is_empty()
                    || text[line_start..start].trim().is_empty()
                {
                    continue;
                }
                let inside_literal = tokens.iter().any(|token| {
                    matches!(
                        token.kind,
                        SyntaxKind::STRING
                            | SyntaxKind::COMMENT
                            | SyntaxKind::FSTRING_MIDDLE
                            | SyntaxKind::TSTRING_MIDDLE
                    ) && u32::from(token.range.start()) as usize <= start
                        && start < u32::from(token.range.end()) as usize
                });
                if inside_literal {
                    continue;
                }
                let before = &text[line_start..start];
                let gap = before[before.trim_end_matches([' ', '\t']).len()..].to_owned();
                let sentinel = format!("{mask}{}_end_anchor__", self.prefix);
                anchors.push((start, end, mask.clone(), sentinel, gap));
            }
        }
        anchors.sort_by_key(|anchor| anchor.0);
        let mut input = text.to_owned();
        for (start, end, _, sentinel, _) in anchors.iter().rev() {
            input.replace_range(*start..*end, &format!("#{sentinel}"));
        }
        let mut output = format(&input).map_err(|error| error.to_string())?;
        for (_, _, mask, sentinel, gap) in anchors {
            let matches = output
                .match_indices(&sentinel)
                .map(|(at, _)| at)
                .collect::<Vec<_>>();
            if matches.len() != 1 {
                return Err("Formatting changed an END anchor".into());
            }
            let start = matches[0];
            let line_start = output[..start].rfind('\n').map_or(0, |at| at + 1);
            let Some(hash) = output[line_start..start]
                .rfind('#')
                .map(|at| line_start + at)
            else {
                return Err("Formatting changed an END anchor".into());
            };
            if !output[hash + 1..start].trim().is_empty() {
                return Err("Formatting changed an END anchor".into());
            }
            let gap_start = output[..hash].trim_end_matches([' ', '\t']).len();
            output.replace_range(gap_start..start + sentinel.len(), &format!("{gap}{mask}"));
        }
        // This checks ordinary masks and their captured order as well as anchors.
        self.finish(&output)?;
        Ok(output)
    }

    /// Formatting may change whitespace, but must retain every live marker.
    /// Missing/duplicated markers refuse creation rather than publish bad stops.
    pub fn finish(&self, text: &str) -> Result<Expansion, String> {
        fn markers(
            text: &str,
            prefix: &str,
            masks: &[(String, String)],
        ) -> Result<Vec<(usize, usize, usize)>, String> {
            let mut result = Vec::new();
            let mut at = 0;
            while let Some(relative) = text[at..].find(prefix) {
                let start = at + relative;
                let digits = start + prefix.len();
                let Some(close) = text[digits..].find("__") else {
                    return Err("A live field was changed by formatting".into());
                };
                let end = digits + close + 2;
                let index = text[digits..digits + close]
                    .parse::<usize>()
                    .map_err(|_| "Invalid live field marker")?;
                if masks
                    .get(index)
                    .is_none_or(|(mask, _)| mask != &text[start..end])
                {
                    return Err("Invalid live field marker".into());
                }
                if result.len() >= 4096 {
                    return Err("Too many live fields in the rendered file".into());
                }
                result.push((start, end, index));
                at = end;
            }
            Ok(result)
        }
        let original = markers(&self.text, &self.prefix, &self.masks)?;
        let current = markers(text, &self.prefix, &self.masks)?;
        if original.iter().map(|x| x.2).collect::<Vec<_>>()
            != current.iter().map(|x| x.2).collect::<Vec<_>>()
        {
            return Err(
                "Formatting changed live fields; disable reformatting or correct the template"
                    .into(),
            );
        }
        let mut out = String::new();
        let mut stops = Vec::new();
        let mut end = None;
        let mut at = 0;
        for (start, finish, index) in current {
            out.push_str(&text[at..start]);
            let name = &self.masks[index].1;
            if name == "END" {
                end = Some(out.len())
            } else if name != "SELECTION" {
                stops.push((out.len(), out.len(), name.clone()))
            }
            at = finish;
        }
        out.push_str(&text[at..]);
        Ok(Expansion {
            text: out,
            tab_stops: stops,
            end_offset: end,
        })
    }
}
fn protect(
    text: &str,
    prefix: &str,
    masks: &mut Vec<(String, String)>,
) -> Result<String, crate::file::TooLarge> {
    if text.len() > crate::file::MAX_RENDER_BYTES {
        return Err(crate::file::TooLarge);
    }
    let mut out = String::new();
    let mut at = 0;
    while at < text.len() {
        let ch = text[at..].chars().next().unwrap();
        if ch == '\\' {
            out.push(ch);
            at += 1;
            if let Some(next) = text[at..].chars().next() {
                out.push(next);
                at += next.len_utf8();
            }
            continue;
        }
        if text[at..].starts_with("$$") {
            out.push_str("$$");
            at += 2;
            continue;
        }
        if ch == '$' {
            let start = at + 1;
            let mut end = start;
            for c in text[start..].chars() {
                if c.is_alphanumeric() || c == '_' {
                    end += c.len_utf8()
                } else {
                    break;
                }
            }
            if end > start && text[end..].starts_with('$') {
                if masks.len() >= 4096 {
                    return Err(crate::file::TooLarge);
                }
                let name = text[start..end].to_string();
                let mask = format!("{prefix}{}__", masks.len());
                out.push_str(&mask);
                masks.push((mask, name));
                at = end + 1;
                continue;
            }
        }
        out.push(ch);
        at += ch.len_utf8();
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::TemplateStore;
    #[test]
    fn velocity_and_live_names_are_separate_and_unicode_offsets_exact() {
        let set = TemplateSet::merged(&TemplateStore::default(), None);
        let t = FileTemplate::new(
            "Mixed",
            "py",
            "mixed",
            "${NAME}:π$NAME$/$NAME$$END$/${VALUE}",
            "",
        );
        let vars = BTreeMap::from([
            ("NAME".into(), "module".into()),
            ("VALUE".into(), "$LITERAL$".into()),
        ]);
        let p = PreparedLive::render(&set, &t, &vars).unwrap();
        let e = p.finish(&p.text).unwrap();
        assert_eq!(e.text, "module:π//$LITERAL$");
        assert_eq!(
            e.tab_stops,
            vec![(9, 9, "NAME".into()), (10, 10, "NAME".into())]
        );
        assert_eq!(e.end_offset, Some(10));
    }
}

#[cfg(test)]
mod coverage {
    use super::*;
    use crate::file::{Origin, TemplateStore};
    #[test]
    fn every_literal_builtin_kind_is_available_and_described() {
        let set = TemplateSet::merged(&TemplateStore::default(), None);
        for name in [
            "Python File",
            "Python Unit Test",
            "Python pytest",
            "setup.py",
            "__init__.py",
            "HTML File",
            "JavaScript File",
            "CSS File",
            "Markdown File",
            "reStructuredText File",
            "YAML File",
            "JSON File",
            ".editorconfig",
            "requirements.txt",
            "Dockerfile",
            ".gitignore",
        ] {
            let template = set
                .template(name)
                .unwrap_or_else(|| panic!("Missing {name}"));
            assert!(!template.description.is_empty());
            assert!(!template.live_templates);
        }
        let legacy: FileTemplate =
            serde_json::from_str(r#"{"name":"Legacy","body":"$FIELD$"}"#).unwrap();
        assert!(!legacy.live_templates);
    }
    #[test]
    fn included_and_conditional_live_fields_keep_velocity_values_literal() {
        let mut store = TemplateStore::default();
        store.includes.push(FileTemplate::new(
            "File Header",
            "py",
            "",
            "# π $AUTHOR$\n",
            "",
        ));
        let set = TemplateSet::merged(&TemplateStore::default(), Some(&store));
        assert_eq!(
            set.includes
                .iter()
                .find(|(t, _)| t.name == "File Header")
                .unwrap()
                .1,
            Origin::Project
        );
        let t=FileTemplate::new("Conditional","py","module","#parse(\"File Header.py\")\n#if (${ON})\n$FIELD$$END$\n#else\n$OMITTED$\n#end\n${VALUE}","");
        let vars = BTreeMap::from([
            ("ON".into(), "true".into()),
            ("VALUE".into(), "$NOT_A_FIELD$".into()),
        ]);
        let p = PreparedLive::render(&set, &t, &vars).unwrap();
        let e = p.finish(&p.text).unwrap();
        assert_eq!(e.text, "# π \n\n$NOT_A_FIELD$");
        assert_eq!(
            e.tab_stops.iter().map(|x| x.2.as_str()).collect::<Vec<_>>(),
            ["AUTHOR", "FIELD"]
        );
        assert_eq!(e.end_offset, Some("# π \n".len()));
    }
    #[test]
    fn formatting_changed_markers_refuse_and_large_field_sets_are_bounded() {
        let set = TemplateSet::merged(&TemplateStore::default(), None);
        let t = FileTemplate::new("Safe", "py", "safe", "π $FIELD$ $END$", "");
        let p = PreparedLive::render(&set, &t, &BTreeMap::new()).unwrap();
        assert!(p.finish("formatter discarded fields").is_err());
        let mut large = t;
        large.body = "$FIELD$".repeat(4097);
        assert!(PreparedLive::render(&set, &large, &BTreeMap::new()).is_err());
    }
    #[test]
    fn python_end_anchors_preserve_literals_comments_middle_gaps_and_identity() {
        let set = TemplateSet::default();
        for (body, expected, anchor) in [
            ("return 'π'$END$\n", "return 'π'\n", true),
            ("return 42  $END$\n", "return 42  \n", true),
            ("return '$END$π'\n", "return 'π'\n", false),
            ("# note $END$\n", "# note \n", false),
            ("return ($END$ + 1)\n", "return ( + 1)\n", false),
        ] {
            let template = FileTemplate::new("Test", "py", "m", body, "");
            let live = PreparedLive::render(&set, &template, &BTreeMap::new()).unwrap();
            let formatted = live
                .format_python(&live.text, |input| {
                    assert_eq!(input.contains("_end_anchor__"), anchor);
                    Ok::<_, String>(input.replace("#", "# "))
                })
                .unwrap();
            assert_eq!(
                live.finish(&formatted).unwrap().text,
                if body.starts_with('#') {
                    "#  note \n"
                } else {
                    expected
                }
            );
            assert!(live
                .format_python(&live.text, |_| Ok::<_, String>(String::new()))
                .is_err());
        }
    }
}
