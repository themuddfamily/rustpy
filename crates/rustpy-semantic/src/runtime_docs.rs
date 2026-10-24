//! Docstrings of builtins and compiled standard-library modules collected
//! from the project interpreter (`rustpy_run::docs`, roadmap §2.10 "Quick
//! Documentation sources"). Typeshed stubs carry no docstrings, so Quick
//! Documentation and Parameter Info of `print`, `str.split`, `math.sqrt` or
//! `itertools.chain` fall back to these.

use std::collections::HashMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeDocs {
    /// `module.qualname` (`builtins.str.split`, `math.sqrt`; `math` for the
    /// module itself) → docstring.
    docs: HashMap<String, String>,
}

impl RuntimeDocs {
    /// Parses a dump `{"format": 1, "version": "3.12.3", "docs": {"builtins.len": "…"}}`
    /// (empty when it is not one).
    pub fn from_json(text: &str) -> Self {
        if text.len() > 32 * 1024 * 1024 {
            return Self::default();
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
            return Self::default();
        };
        if v.get("format")
            .is_some_and(|format| format.as_u64() != Some(1))
            || v["docs"].as_object().is_some_and(|docs| docs.len() > 32768)
        {
            return Self::default();
        }
        let docs = v["docs"]
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, d)| {
                        let doc = d.as_str()?;
                        (k.len() <= 1024 && doc.len() <= 32768)
                            .then(|| (k.clone(), doc.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self { docs }
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// The docstring of `qualname` (`["str", "split"]`; empty for the module
    /// itself) in `module`.
    pub fn get(&self, module: &str, qualname: &[String]) -> Option<&str> {
        let key = if qualname.is_empty() {
            module.to_string()
        } else {
            format!("{module}.{}", qualname.join("."))
        };
        self.docs.get(&key).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_dump() {
        let d = RuntimeDocs::from_json(
            r#"{"docs": {"builtins.len": "Return the number of items in a container.", "math": "Mathematical functions.", "bad": 3}, "format": 1}"#,
        );
        assert_eq!(d.len(), 2);
        assert_eq!(
            d.get("builtins", &["len".to_string()]),
            Some("Return the number of items in a container.")
        );
        assert_eq!(d.get("math", &[]), Some("Mathematical functions."));
        assert_eq!(
            d.get("builtins", &["str".to_string(), "split".to_string()]),
            None
        );
        assert!(RuntimeDocs::from_json("not json").is_empty());
    }

    #[test]
    fn quick_documentation_and_parameter_info_use_them() {
        use crate::db::Analysis;
        use rustpy_core::TextSize;
        use std::path::Path;
        let db = Analysis::for_project(Path::new("/proj"));
        let src = "import math\nimport itertools\n\nprint(len('a b'.split()), math.sqrt(2), itertools.chain([1]))\nlen(\n";
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let f = db.file(id).unwrap();
        let at = |s: &str| TextSize::new(src.find(s).unwrap() as u32);
        let doc = |s: &str| crate::features::hover_at(&db, &f, at(s)).and_then(|h| h.docstring);
        // Typeshed has no docstrings for these.
        assert_eq!(doc("len("), None);
        assert_eq!(doc("sqrt"), None);
        let dump = r#"{"format": 1, "docs": {"builtins.len": "Return the number of items in a container.", "builtins.str.split": "Return a list of the substrings in the string.", "math.sqrt": "Return the square root of x.", "itertools.chain": "Return a chain object.", "math": "Mathematical functions."}}"#;
        db.set_runtime_docs(std::sync::Arc::new(RuntimeDocs::from_json(dump)));
        assert_eq!(
            doc("len(").as_deref(),
            Some("Return the number of items in a container.")
        );
        assert_eq!(
            doc("split").as_deref(),
            Some("Return a list of the substrings in the string.")
        );
        assert_eq!(doc("sqrt").as_deref(), Some("Return the square root of x."));
        assert_eq!(doc("chain").as_deref(), Some("Return a chain object."));
        assert_eq!(doc("math.sqrt").as_deref(), Some("Mathematical functions."));
        // A module member names its module.
        assert_eq!(
            crate::features::hover_at(&db, &f, at("sqrt"))
                .and_then(|h| h.origin)
                .as_deref(),
            Some("math")
        );
        // Parameter Info shows the docstring too.
        let help = crate::features::signature_help(&db, &f, TextSize::new(src.len() as u32 - 1))
            .expect("parameter info");
        assert_eq!(
            help.docstring.as_deref(),
            Some("Return the number of items in a container.")
        );
    }
}
