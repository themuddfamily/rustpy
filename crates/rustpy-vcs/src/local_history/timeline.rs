//! Bounded directory events, selection projection, patch export and retention.
use super::*;
use std::io::{Read, Seek, SeekFrom, Write};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    Created,
    Opened,
    Deleted,
    Rename { from: PathBuf },
    Changed,
    Saved,
    Label,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEvent {
    pub path: PathBuf,
    pub kind: EventKind,
    pub time: i64,
    pub label: String,
}
pub fn record_events(
    base: &Path,
    root: Option<&Path>,
    events: &[HistoryEvent],
) -> std::io::Result<()> {
    if !policy().enabled || events.is_empty() {
        return Ok(());
    }
    let _guard = WRITE_LOCK
        .lock()
        .map_err(|_| std::io::Error::other("History lock unavailable"))?;
    let directory = project_dir(base, root);
    std::fs::create_dir_all(&directory)?;
    let path = directory.join("events.jsonl");
    if path.metadata().is_ok_and(|meta| meta.len() > 1024 * 1024) {
        let old = read_events(base, root);
        let mut file = std::fs::File::create(&path)?;
        for event in old
            .iter()
            .rev()
            .take(2048)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            serde_json::to_writer(&mut file, event).map_err(std::io::Error::other)?;
            file.write_all(b"\n")?;
        }
    }
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)?;
    for event in events.iter().take(1024) {
        serde_json::to_writer(&mut file, event).map_err(std::io::Error::other)?;
        file.write_all(b"\n")?;
    }
    Ok(())
}
pub fn read_events(base: &Path, root: Option<&Path>) -> Vec<HistoryEvent> {
    let Ok(mut file) = std::fs::File::open(project_dir(base, root).join("events.jsonl")) else {
        return Vec::new();
    };
    let mut bytes = Vec::new();
    let length = file
        .metadata()
        .map(|metadata| metadata.len())
        .unwrap_or_default();
    if length > 2 * 1024 * 1024 && file.seek(SeekFrom::End(-2 * 1024 * 1024)).is_err() {
        return Vec::new();
    }
    if file.take(2 * 1024 * 1024).read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect::<Vec<HistoryEvent>>()
        .into_iter()
        .rev()
        .take(8192)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// Follow current line positions back across insertions/deletions. Inserted
/// lines have no older text; deleted old lines at the selection are retained.
pub fn selection_text_at(old: &str, current: &str, start_line: usize, end_line: usize) -> String {
    use similar::{ChangeTag, TextDiff};
    let mut line = 0;
    let mut selected = String::new();
    let diff = TextDiff::from_lines(old, current);
    for change in diff.iter_all_changes() {
        if (start_line..=end_line).contains(&line) && change.tag() != ChangeTag::Insert {
            selected.push_str(change.value());
        }
        if change.tag() != ChangeTag::Delete {
            line += 1;
        }
    }
    selected
}
pub fn label_paths(base: &Path, root: Option<&Path>, paths: &[PathBuf], label: &str) -> usize {
    let mut files = Vec::new();
    for path in paths {
        collect_files(path, &mut files);
    }
    let mut written = 0;
    let mut bytes = 0;
    for file in files.into_iter().take(MAX_FILES_PER_CALL) {
        if let Some(text) = read_text(&file) {
            bytes += text.len();
            if bytes > MAX_BYTES_PER_CALL {
                break;
            }
            if label_text(base, root, &file, &text, label) {
                written += 1;
            }
        }
    }
    written
}
pub fn create_patch(path: &Path, old: &str, current: &str) -> String {
    let path = path.to_string_lossy().replace(['\n', '\r'], "_");
    similar::TextDiff::from_lines(old, current)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string()
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pruned {
    pub removed: usize,
    pub bytes: u64,
    pub remaining_bytes: u64,
}
/// Evict oldest snapshots by age and total size under the same writer lock.
/// Journal files and metadata are retained to explain deleted/renamed paths.
pub fn prune(
    base: &Path,
    settings: &rustpy_settings::local_history::LocalHistorySettings,
    now: i64,
) -> std::io::Result<Pruned> {
    let _guard = WRITE_LOCK
        .lock()
        .map_err(|_| std::io::Error::other("History lock unavailable"))?;
    let mut snapshots = Vec::new();
    let mut total = 0u64;
    let projects = match std::fs::read_dir(base) {
        Ok(projects) => projects,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Pruned::default()),
        Err(error) => return Err(error),
    };
    for project in projects.flatten() {
        if !project.file_type()?.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for file in files.flatten() {
            if !file.file_type()?.is_dir() {
                continue;
            }
            for entry in list_entries(&file.path()) {
                let metadata = std::fs::symlink_metadata(&entry.file)?;
                if !metadata.is_file() {
                    continue;
                }
                total = total.saturating_add(metadata.len());
                snapshots.push((entry, metadata.len()));
                if snapshots.len() > 100_000 {
                    return Err(std::io::Error::other(
                        "History cleanup exceeds 100,000 snapshots",
                    ));
                }
            }
        }
    }
    snapshots.sort_by_key(|(entry, _)| entry.time);
    let cutoff = now.saturating_sub(i64::from(settings.retention_days.clamp(1, 3650)) * 86_400_000);
    let maximum = u64::from(settings.max_size_mb.clamp(1, 1024 * 1024)) * 1024 * 1024;
    let mut result = Pruned::default();
    for (entry, bytes) in snapshots {
        if entry.time >= cutoff && total <= maximum {
            continue;
        }
        std::fs::remove_file(entry.file)?;
        result.removed += 1;
        result.bytes = result.bytes.saturating_add(bytes);
        total = total.saturating_sub(bytes);
    }
    result.remaining_bytes = total;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_follows_insertions_and_patch_preserves_content() {
        let old = "header\nvalue = 1\ntail\n";
        let current = "new\nheader\nvalue = 2\ntail\n";
        assert_eq!(selection_text_at(old, current, 2, 2), "value = 1\n");
        let patch = create_patch(Path::new("main.py"), old, current);
        assert!(patch.contains("-value = 1"));
        assert!(patch.contains("+value = 2"));
    }
    #[test]
    fn labels_survive_identical_text_and_directory_events_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path();
        let root = Path::new("project");
        let file = root.join("main.py");
        assert!(label_text(
            base,
            Some(root),
            &file,
            "one",
            "First checkpoint"
        ));
        assert!(label_text(
            base,
            Some(root),
            &file,
            "one",
            "Second checkpoint"
        ));
        assert_eq!(list_entries(&file_dir(base, Some(root), &file)).len(), 2);
        let events = vec![HistoryEvent {
            path: file.clone(),
            kind: EventKind::Rename {
                from: root.join("old.py"),
            },
            time: 42,
            label: "Rename".into(),
        }];
        record_events(base, Some(root), &events).unwrap();
        assert_eq!(read_events(base, Some(root)), events);
    }
    #[test]
    fn retention_and_storage_bounds_evict_oldest_snapshots() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path();
        let file = Path::new("a.py");
        let settings = rustpy_settings::local_history::LocalHistorySettings {
            retention_days: 1,
            max_size_mb: 1,
            ..Default::default()
        };
        let text = "x".repeat(600_000);
        assert!(store_with_policy(
            base,
            None,
            file,
            &text,
            "old",
            SnapshotMode {
                deleted: false,
                force: true
            },
            &settings
        ));
        assert!(store_with_policy(
            base,
            None,
            file,
            &text,
            "new",
            SnapshotMode {
                deleted: false,
                force: true
            },
            &settings
        ));
        let result = prune(base, &settings, chrono::Local::now().timestamp_millis()).unwrap();
        assert_eq!(result.removed, 1);
        assert!(result.remaining_bytes <= 1024 * 1024);
        let result = prune(
            base,
            &settings,
            chrono::Local::now().timestamp_millis() + 2 * 86_400_000,
        )
        .unwrap();
        assert_eq!(result.remaining_bytes, 0);
    }
}
