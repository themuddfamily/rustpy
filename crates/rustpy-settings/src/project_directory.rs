//! Machine-local project creation authority. Validation never creates directories.
use std::path::{Component, Path, PathBuf};

pub fn validate(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("Default project directory must be an absolute path".into());
    }
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("Project directory must not contain parent (..) components".into());
    }
    let mut ancestor = path;
    let mut missing = Vec::new();
    loop {
        match std::fs::metadata(ancestor) {
            Ok(metadata) => {
                if !metadata.is_dir() {
                    return Err("Project directory has a non-directory ancestor".into());
                }
                let mut canonical = dunce::canonicalize(ancestor).map_err(|e| e.to_string())?;
                for name in missing.iter().rev() {
                    canonical.push(name);
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(
                    ancestor
                        .file_name()
                        .ok_or("Project directory has no existing ancestor")?
                        .to_os_string(),
                );
                ancestor = ancestor
                    .parent()
                    .ok_or("Project directory has no existing ancestor")?;
            }
            Err(error) => return Err(format!("Cannot access project directory: {error}")),
        }
    }
}

pub fn creation_directory(
    configured: Option<&Path>,
    project: Option<&Path>,
    home: Option<PathBuf>,
) -> PathBuf {
    configured
        .map(Path::to_path_buf)
        .or_else(|| project.and_then(Path::parent).map(Path::to_path_buf))
        .or(home)
        .unwrap_or_else(|| PathBuf::from("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configured_missing_parent_is_not_replaced_by_home() {
        let chosen = PathBuf::from("/chosen/not-created/projects");
        assert_eq!(
            creation_directory(
                Some(&chosen),
                Some(Path::new("/active/project")),
                Some("/home/person".into())
            ),
            chosen
        );
        assert_eq!(
            creation_directory(
                None,
                Some(Path::new("/active/project")),
                Some("/home/person".into())
            ),
            PathBuf::from("/active")
        );
        assert_eq!(
            creation_directory(None, None, Some("/home/person".into())),
            PathBuf::from("/home/person")
        );
    }
    #[test]
    fn invalid_explicit_paths_are_rejected_before_filesystem_access() {
        assert!(validate(Path::new("relative/projects")).is_err());
        assert!(validate(Path::new("/projects/../outside")).is_err());
    }
}
