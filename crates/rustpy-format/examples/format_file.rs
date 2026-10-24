//! Dev tool: print the formatted version of Python files.
//! Usage: cargo run -p rustpy-format --example format_file -- [--pep8] [--check] <files...>
//!
//! `--check` only reports which files would change (exit code 1 if any).
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let pep8 = args.iter().any(|a| a == "--pep8");
    let check = args.iter().any(|a| a == "--check");
    let opts = if pep8 {
        rustpy_format::FormatOptions::pep8()
    } else {
        rustpy_format::FormatOptions::black()
    };
    let mut changed = 0;
    for path in args.iter().filter(|a| !a.starts_with("--")) {
        let src = std::fs::read_to_string(path).expect("read file");
        match rustpy_format::format_source(&src, &opts) {
            Ok(out) => {
                if check {
                    if out != src {
                        println!("would reformat {path}");
                        changed += 1;
                    }
                } else {
                    print!("{out}");
                }
            }
            Err(e) => eprintln!("{path}: {e}"),
        }
    }
    if changed > 0 {
        std::process::exit(1);
    }
}
