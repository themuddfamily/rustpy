//! Command lines shown for editing in the UI.

/// Joins arguments into a parameter string that `rustpy_project::RunConfig::split_args`
/// splits back.
pub fn quote_args(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if !a.is_empty() && !a.contains(|c: char| c.is_whitespace() || c == '"' || c == '\'') {
                a.clone()
            } else if a.contains('"') {
                format!("'{a}'")
            } else {
                format!("\"{a}\"")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
