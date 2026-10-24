//! Headless HTTP client uses the same parser, environments, scripts and transports.
use crate::args::HttpArgs;
use std::io::Write;
pub fn run(args: &HttpArgs, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let settings = rustpy_settings::Settings::load();
    rustpy_net::configure(rustpy_net::NetConfig {
        proxy: settings.http_proxy,
        certificates: settings.certificates,
        certs_dir: Some(rustpy_settings::Settings::config_dir().join("certs")),
    });
    let text = match rustpy_core::vfs::Vfs::new().read(
        &args.file,
        rustpy_core::vfs::ReadLimits {
            max_file_bytes: 2 * 1024 * 1024,
            max_cached_bytes: 0,
            max_cached_files: 0,
        },
    ) {
        Ok(snapshot) => match String::from_utf8(snapshot.bytes.to_vec()) {
            Ok(text) => text,
            Err(error) => {
                let _ = writeln!(err, "HTTP file must be UTF-8: {error}");
                return 2;
            }
        },
        Err(error) => {
            let _ = writeln!(err, "Cannot read HTTP file: {error}");
            return 2;
        }
    };
    let base = args.file.parent().unwrap_or(std::path::Path::new("."));
    let mut options = rustpy_http::runtime::Options::new(base.into());
    options.timeout = std::time::Duration::from_millis(args.timeout_ms.clamp(1, 300_000));
    options.variables = rustpy_http::load_env(base, &args.environment);
    options.variables.extend(rustpy_http::file_variables(&text));
    options.variables.extend(args.variables.clone());
    let requests = rustpy_http::parse(&text);
    let mut found = false;
    let mut status = 0;
    for (index, request) in requests.into_iter().enumerate() {
        if args.request.as_ref().is_some_and(|selected| {
            selected != &(index + 1).to_string() && request.name.as_ref() != Some(selected)
        }) {
            continue;
        }
        found = true;
        let completed = rustpy_http::runtime::execute(
            request,
            options.clone(),
            &rustpy_http::runtime::Control::default(),
        );
        options.globals = completed.globals;
        if let Err(error) =
            rustpy_http::append_history(base, &completed.request, &completed.response)
        {
            let _ = writeln!(err, "Cannot write HTTP history: {error}");
        }
        if args.json {
            let _ = writeln!(
                out,
                "{}",
                serde_json::to_string(&completed.response).unwrap_or_default()
            );
        } else {
            let _ = writeln!(
                out,
                "{} {} — {}",
                completed.request.method,
                completed.request.url,
                completed.response.status_line()
            );
            for (key, value) in &completed.response.headers {
                let _ = writeln!(out, "{key}: {value}");
            }
            let _ = writeln!(out, "\n{}", completed.response.body);
            for test in &completed.response.tests {
                let _ = writeln!(
                    out,
                    "{} {} {}",
                    if test.passed { "PASS" } else { "FAIL" },
                    test.name,
                    test.message
                );
            }
        }
        if completed.response.error.is_some() || completed.response.status >= 400 {
            status = 1;
        }
    }
    if !found {
        let _ = writeln!(err, "No matching HTTP request");
        2
    } else {
        status
    }
}
