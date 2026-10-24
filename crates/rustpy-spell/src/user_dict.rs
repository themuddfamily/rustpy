//! Persistence of user and project dictionaries ("Save to dictionary").
//!
//! A dictionary file is UTF-8 text with one word per line. Blank lines and
//! lines starting with `#` are ignored. Hunspell `.dic` files load too: a
//! leading line holding only the word count is skipped and `/FLAGS` affix
//! suffixes are dropped (affix rules are not applied).

use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// The word on one dictionary line, if the line holds one.
fn line_word(line: &str) -> Option<&str> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let word = line.split('/').next().unwrap_or_default().trim();
    (!word.is_empty()).then_some(word)
}

/// The words of a dictionary file's content, deduplicated case-insensitively.
fn parse_words(content: &str) -> Vec<String> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut seen = std::collections::HashSet::new();
    let mut words = Vec::new();
    let mut first = true;
    for line in content.lines() {
        let Some(word) = line_word(line) else {
            continue;
        };
        // Hunspell `.dic` header: the number of entries.
        let is_count_header = first && word.bytes().all(|b| b.is_ascii_digit());
        first = false;
        if is_count_header {
            continue;
        }
        if seen.insert(word.to_lowercase()) {
            words.push(word.to_string());
        }
    }
    words
}

fn read_existing(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e),
    }
}

/// Loads the words of a user or project dictionary file. A missing file is an
/// empty dictionary. Words keep their spelling; duplicates (ignoring case) are
/// dropped. Pass the result to [`crate::Dictionary::with_user_words`].
pub fn load_user_words(path: impl AsRef<Path>) -> io::Result<Vec<String>> {
    Ok(parse_words(&read_existing(path.as_ref())?))
}

fn validate(word: &str) -> io::Result<&str> {
    let word = word.trim();
    let invalid = word.is_empty()
        || word.chars().any(|c| c.is_whitespace() || c.is_control())
        || word.contains('/')
        || word.starts_with('#')
        || word.bytes().all(|b| b.is_ascii_digit());
    if invalid {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not a dictionary word: {word:?}"),
        ));
    }
    Ok(word)
}

/// Adds `word` to the dictionary file at `path`, creating the file and its
/// parent directories as needed. Returns `Ok(false)` when the file already has
/// the word (ignoring case). Words must be non-empty and contain no
/// whitespace, `/` or leading `#`.
pub fn add_user_word(path: impl AsRef<Path>, word: &str) -> io::Result<bool> {
    let path = path.as_ref();
    let word = validate(word)?;
    let content = read_existing(path)?;
    let lower = word.to_lowercase();
    if parse_words(&content)
        .iter()
        .any(|w| w.to_lowercase() == lower)
    {
        return Ok(false);
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let separator = if content.is_empty() || content.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    file.write_all(format!("{separator}{word}\n").as_bytes())?;
    Ok(true)
}

/// Removes `word` (ignoring case) from the dictionary file at `path`, keeping
/// every other line. Returns `Ok(false)` when the file does not exist or does
/// not contain the word.
pub fn remove_user_word(path: impl AsRef<Path>, word: &str) -> io::Result<bool> {
    let path = path.as_ref();
    let content = read_existing(path)?;
    let lower = word.trim().to_lowercase();
    let mut removed = false;
    let mut kept = String::with_capacity(content.len());
    for line in content.split_inclusive('\n') {
        if line_word(line).is_some_and(|w| w.to_lowercase() == lower) {
            removed = true;
        } else {
            kept.push_str(line);
        }
    }
    if !removed {
        return Ok(false);
    }
    // Write a sibling file and rename it over the original, so that a crash
    // never leaves a truncated dictionary behind.
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    fs::write(&tmp, kept)?;
    fs::rename(&tmp, path)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load_user_words(dir.path().join("nope.dic")).unwrap(),
            Vec::<String>::new()
        );
        assert!(!remove_user_word(dir.path().join("nope.dic"), "word").unwrap());
    }

    #[test]
    fn loads_plain_and_hunspell_files() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("words.txt");
        fs::write(
            &plain,
            "\u{feff}GitLab\n# a comment\n\n  rustpy  \ngitlab\nKwargs\r\n",
        )
        .unwrap();
        assert_eq!(
            load_user_words(&plain).unwrap(),
            ["GitLab", "rustpy", "Kwargs"]
        );
        let dic = dir.path().join("project.dic");
        fs::write(&dic, "3\nfoo/SM\nbar\n/X\nbaz/\n").unwrap();
        assert_eq!(load_user_words(&dic).unwrap(), ["foo", "bar", "baz"]);
        // Only the first line can be a count header.
        let numbers = dir.path().join("numbers.dic");
        fs::write(&numbers, "word\n42\n").unwrap();
        assert_eq!(load_user_words(&numbers).unwrap(), ["word", "42"]);
    }

    #[test]
    fn adds_words_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("dir").join("user.dic");
        assert!(add_user_word(&path, "GitLab").unwrap());
        assert!(!add_user_word(&path, "gitlab").unwrap());
        assert!(add_user_word(&path, " rustpy ").unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "GitLab\nrustpy\n");
        assert_eq!(load_user_words(&path).unwrap(), ["GitLab", "rustpy"]);
    }

    #[test]
    fn appends_after_a_missing_final_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("user.dic");
        fs::write(&path, "first").unwrap();
        assert!(add_user_word(&path, "second").unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "first\nsecond\n");
    }

    #[test]
    fn rejects_invalid_words() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("user.dic");
        for bad in ["", "   ", "two words", "a/b", "#tag", "123", "tab\tbed"] {
            let err = add_user_word(&path, bad).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
        }
        assert!(!path.exists());
    }

    #[test]
    fn removes_words_keeping_other_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("user.dic");
        fs::write(&path, "# mine\nalpha\nBeta/X\ngamma").unwrap();
        assert!(remove_user_word(&path, "beta").unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "# mine\nalpha\ngamma");
        assert!(!remove_user_word(&path, "beta").unwrap());
        assert!(remove_user_word(&path, "GAMMA").unwrap());
        assert_eq!(load_user_words(&path).unwrap(), ["alpha"]);
        assert!(!dir.path().join("user.dic.tmp").exists());
    }
}
