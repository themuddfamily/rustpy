//! Proposed examples/cpython_ast.rs; exporter is independent of Python at runtime.
#[path = "support/cpython_ast.rs"]
mod cpython_ast;
#[path = "support/source_decode.rs"]
mod source_decode;
fn main() {
    let path = std::env::args_os().nth(1).expect("source path");
    let bytes = std::fs::read(path).expect("source read");
    let text = match source_decode::decode(&bytes) {
        Ok(text) => text,
        Err(source_decode::DecodeError::Invalid(message)) => {
            eprintln!("Invalid source: {message}");
            println!("{}", serde_json::json!({"accepted":false}));
            return;
        }
        Err(source_decode::DecodeError::Unsupported(message)) => {
            eprintln!("Unsupported: {message}");
            std::process::exit(2);
        }
    };
    match cpython_ast::export(&text) {
        Ok(Some(ast)) => println!("{}", serde_json::json!({"accepted":true,"ast":ast})),
        Ok(None) => println!("{}", serde_json::json!({"accepted":false})),
        Err(error) => {
            eprintln!("Unsupported: {}", error.0);
            std::process::exit(2);
        }
    }
}
