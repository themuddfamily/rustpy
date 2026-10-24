//! Dev tool: parse files and print syntax errors with line numbers.
//! Usage: cargo run -p rustpy-syntax --example parse_file -- [--tree] <files...>
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let show_tree = args.iter().any(|a| a == "--tree");
    for path in args.iter().filter(|a| !a.starts_with("--")) {
        let src = std::fs::read_to_string(path).expect("read file");
        let parse = rustpy_syntax::parse(&src);
        if show_tree {
            println!(
                "{}",
                rustpy_syntax::parser::debug_tree(&parse.syntax_node())
            );
        }
        for e in parse.errors() {
            let start: usize = e.range.start().into();
            let line = src[..start].matches('\n').count() + 1;
            let line_start = src[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let col = start - line_start + 1;
            let line_text = src[line_start..].lines().next().unwrap_or("");
            println!(
                "{path}:{line}:{col}: {}\n    {}",
                e.message,
                line_text.trim_end()
            );
        }
        println!("{path}: {} errors", parse.errors().len());
    }
}
