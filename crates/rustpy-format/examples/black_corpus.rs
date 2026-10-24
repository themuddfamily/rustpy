//! Serial, offline assessment of the complete pinned Black cases directory.
#[path = "../tests/support/black_corpus.rs"]
mod corpus;
fn main() {
    let require_exact = std::env::args().skip(1).any(|arg| arg == "--require-exact");
    let report = corpus::assess().unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(2)
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("serialize assessment")
    );
    if report.unstable != 0
        || report.comments_changed != 0
        || (require_exact && report.exact != report.total)
    {
        std::process::exit(1);
    }
}
