//! Three-way merge of texts (`git merge-file`), for `rustpy merge`.

/// Merges the changes `ours` and `theirs` made to `base`. Returns the merged
/// text and whether it merged cleanly. Conflicts are marked like Git's
/// (`<<<<<<< ours`, `=======`, `>>>>>>> theirs`, with `||||||| base` and the base
/// lines in between when `diff3`), labelled with `labels` (ours, base, theirs).
pub fn merge_texts(
    base: &str,
    ours: &str,
    theirs: &str,
    labels: [&str; 3],
    diff3: bool,
) -> anyhow::Result<(String, bool)> {
    // `git merge-file -p` (libgit2's in-memory merge_file is not usable here:
    // it fails without an error message).
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rustpy-merge-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let result = (|| {
        let (o, b, t) = (dir.join("ours"), dir.join("base"), dir.join("theirs"));
        std::fs::write(&o, ours)?;
        std::fs::write(&b, base)?;
        std::fs::write(&t, theirs)?;
        let mut cmd = std::process::Command::new("git");
        cmd.arg("merge-file").arg("-p");
        if diff3 {
            cmd.arg("--diff3");
        }
        cmd.args(["-L", labels[0], "-L", labels[1], "-L", labels[2]])
            .arg(&o)
            .arg(&b)
            .arg(&t);
        let out = cmd
            .output()
            .map_err(|e| anyhow::anyhow!("cannot run git merge-file: {e}"))?;
        // Exit status: 0 clean, the number of conflicts (> 0), or an error (< 0, 255 on Unix).
        match out.status.code() {
            Some(0) => Ok((String::from_utf8_lossy(&out.stdout).into_owned(), true)),
            Some(c) if (1..=127).contains(&c) => {
                Ok((String::from_utf8_lossy(&out.stdout).into_owned(), false))
            }
            _ => Err(anyhow::anyhow!(
                "git merge-file failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )),
        }
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_separate_changes_and_marks_conflicts() {
        let base = "a\nb\nc\nd\ne\n";
        let (merged, clean) = merge_texts(
            base,
            "A\nb\nc\nd\ne\n",
            "a\nb\nc\nd\nE\n",
            ["left", "base", "right"],
            false,
        )
        .unwrap();
        assert!(clean);
        assert_eq!(merged, "A\nb\nc\nd\nE\n");
        let (merged, clean) = merge_texts(
            base,
            "a\nB1\nc\nd\ne\n",
            "a\nB2\nc\nd\ne\n",
            ["left", "base", "right"],
            true,
        )
        .unwrap();
        assert!(!clean);
        assert_eq!(
            merged,
            "a\n<<<<<<< left\nB1\n||||||| base\nb\n=======\nB2\n>>>>>>> right\nc\nd\ne\n"
        );
        let (merged, _) = merge_texts(
            base,
            "a\nB1\nc\nd\ne\n",
            "a\nB2\nc\nd\ne\n",
            ["left", "base", "right"],
            false,
        )
        .unwrap();
        assert_eq!(
            merged,
            "a\n<<<<<<< left\nB1\n=======\nB2\n>>>>>>> right\nc\nd\ne\n"
        );
    }
}
