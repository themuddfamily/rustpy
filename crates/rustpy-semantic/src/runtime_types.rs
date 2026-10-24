//! Run-time type information collected by the `typecollect.py` helper
//! (Settings › Python Debugger › "Collect run-time types information").
//! Used as a fallback for unannotated parameters and returns.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FunctionTypes {
    /// Parameter name → observed type names (`int`, `str`, `pkg.mod.Class`, `NoneType`).
    pub params: HashMap<String, Vec<String>>,
    pub returns: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct RuntimeTypes {
    /// `(canonical file path, qualified name)` → observed types.
    pub functions: HashMap<(PathBuf, String), FunctionTypes>,
}

fn canonical(p: &Path) -> PathBuf {
    dunce::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

impl RuntimeTypes {
    pub fn path_for(project_root: &Path) -> PathBuf {
        project_root.join(".rustpy").join("runtime_types.json")
    }

    /// Parses `{"functions": {"<file>::<qualname>": {"params": {...}, "return": [...]}}}`.
    pub fn from_json(text: &str) -> Self {
        let mut out = RuntimeTypes::default();
        let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
            return out;
        };
        let Some(funcs) = v["functions"].as_object() else {
            return out;
        };
        for (key, entry) in funcs {
            let Some((file, qual)) = key.rsplit_once("::") else {
                continue;
            };
            let strings = |v: &serde_json::Value| -> Vec<String> {
                v.as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|s| s.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let params = entry["params"]
                .as_object()
                .map(|m| m.iter().map(|(k, v)| (k.clone(), strings(v))).collect())
                .unwrap_or_default();
            out.functions.insert(
                (canonical(Path::new(file)), qual.to_string()),
                FunctionTypes {
                    params,
                    returns: strings(&entry["return"]),
                },
            );
        }
        out
    }

    pub fn load(project_root: &Path) -> Self {
        std::fs::read_to_string(Self::path_for(project_root))
            .map(|t| Self::from_json(&t))
            .unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.functions.is_empty()
    }

    pub fn function(&self, file: &Path, qualname: &str) -> Option<&FunctionTypes> {
        self.functions.get(&(canonical(file), qualname.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_collector_output() {
        let dir = std::env::temp_dir().join(format!("rustpy-rt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("m.py");
        std::fs::write(&file, "def f(x): pass\n").unwrap();
        let json = format!("{{\"version\":1,\"functions\":{{\"{}::C.method\":{{\"params\":{{\"x\":[\"int\",\"str\"]}},\"return\":[\"NoneType\"]}}}}}}", file.display());
        let rt = RuntimeTypes::from_json(&json);
        let f = rt.function(&file, "C.method").expect("entry");
        assert_eq!(f.params["x"], vec!["int".to_string(), "str".to_string()]);
        assert_eq!(f.returns, vec!["NoneType".to_string()]);
        assert!(rt.function(&file, "other").is_none());
        assert!(RuntimeTypes::from_json("not json").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
