//! Lists the translatable UI strings extracted from the `rustpy-ui` sources
//! and checks every locale catalog against them.
//!
//! ```text
//! cargo run -p rustpy-i18n --example i18n_extract                  # summary + coverage
//! cargo run -p rustpy-i18n --example i18n_extract -- --check       # same, exit 1 on gaps
//! cargo run -p rustpy-i18n --example i18n_extract -- --list        # every string, by origin
//! cargo run -p rustpy-i18n --example i18n_extract -- --missing ja  # JSON stub of what ja lacks
//! ```
//!
//! The summary is a report: untranslated strings show in English, so gaps
//! are listed but the exit status is 0. With `--check` it exits with status 1
//! when `locales/en.json` or a complete locale (zh-CN, ja, ko, de, fr, es)
//! misses a string (`cargo test -p rustpy-i18n -- --ignored` runs the same
//! check). The `--missing` stub lists the untranslated strings with empty
//! values (an empty translation falls back to English) ready to be filled in
//! and merged into `locales/<code>.json`.

#[path = "../tests/support/mod.rs"]
mod support;

use std::collections::HashSet;
use std::process::ExitCode;

use support::{read_catalog, Entry, UiStrings, COMPLETE_LOCALES, PARTIAL_LOCALES};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let ui = match UiStrings::from_workspace() {
        Ok(ui) => ui,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    match args.first().map(String::as_str) {
        Some("--list") => {
            for (origin, strings) in ui.groups() {
                println!("# {origin}: {}", strings.len());
                for s in strings {
                    println!("{s}");
                }
            }
            ExitCode::SUCCESS
        }
        Some("--missing") => match args.get(1) {
            Some(code) => print_missing(&ui, code),
            None => {
                eprintln!("usage: i18n_extract --missing <locale>");
                ExitCode::FAILURE
            }
        },
        Some("--check") => summary(&ui, true),
        Some(other) => {
            eprintln!("unknown argument {other:?}; expected --check, --list or --missing <locale>");
            ExitCode::FAILURE
        }
        None => summary(&ui, false),
    }
}

/// Strings every catalog should translate: the source catalog (`en.json`)
/// plus anything extracted from the UI that it does not list yet.
fn source_strings(ui: &UiStrings) -> Result<Vec<String>, String> {
    let mut keys: Vec<String> = read_catalog("en")?.into_iter().map(|(k, _)| k).collect();
    let known: HashSet<String> = keys.iter().cloned().collect();
    keys.extend(
        ui.all()
            .into_iter()
            .filter(|s| !known.contains(*s))
            .map(str::to_owned),
    );
    Ok(keys)
}

fn translated(code: &str) -> Result<HashSet<String>, String> {
    Ok(read_catalog(code)?
        .into_iter()
        .filter(|(_, entry)| match entry {
            Entry::Text(s) => !s.is_empty(),
            Entry::Plural(forms) => forms.values().any(|f| !f.is_empty()),
        })
        .map(|(k, _)| k)
        .collect())
}

fn summary(ui: &UiStrings, check: bool) -> ExitCode {
    println!("Extracted from crates/rustpy-ui:");
    for (origin, strings) in ui.groups() {
        println!("  {:>4}  {origin}", strings.len());
    }
    println!("  {:>4}  distinct strings", ui.all().len());
    let source = match source_strings(ui) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let en: HashSet<String> = read_catalog("en")
        .map(|c| c.into_iter().map(|(k, _)| k).collect())
        .unwrap_or_default();
    let unlisted = ui.all().into_iter().filter(|s| !en.contains(*s)).count();
    println!("\nSource catalog locales/en.json: {} strings ({unlisted} extracted strings not listed yet)", en.len());
    println!("\nCoverage:");
    let mut failed = unlisted > 0;
    for code in COMPLETE_LOCALES.iter().chain(PARTIAL_LOCALES) {
        let done = match translated(code) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        };
        let missing: Vec<&String> = source.iter().filter(|s| !done.contains(*s)).collect();
        let complete = COMPLETE_LOCALES.contains(code);
        let pct = 100.0 * (source.len() - missing.len()) as f64 / source.len().max(1) as f64;
        println!(
            "  {code:<6} {:>5.1}%  {} missing{}",
            pct,
            missing.len(),
            if complete {
                ""
            } else {
                "  (partial locale: falls back to English)"
            }
        );
        if complete && !missing.is_empty() {
            failed = true;
            for s in missing.iter().take(20) {
                println!("           - {s}");
            }
        }
    }
    if !ui.untranslated.is_empty() {
        println!(
            "\nMenu bar literals shown without translation (wrap them in crate::i18n::tr_static):"
        );
        for s in &ui.untranslated {
            println!("           - {s}");
        }
    }
    if failed {
        println!(
            "\nMissing strings show in English. Add them to locales/en.json and translate them in"
        );
        println!("every complete locale (`--missing <locale>` prints a JSON stub).");
    }
    if failed && check {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn print_missing(ui: &UiStrings, code: &str) -> ExitCode {
    let (source, done) = match (source_strings(ui), translated(code)) {
        (Ok(s), Ok(d)) => (s, d),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let stub: serde_json::Map<String, serde_json::Value> = source
        .into_iter()
        .filter(|s| !done.contains(s))
        .map(|s| (s, serde_json::Value::String(String::new())))
        .collect();
    match serde_json::to_string_pretty(&stub) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
