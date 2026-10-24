//! PAC loading, ECMAScript evaluation and ordered proxy transport failover.
//! Standard helper semantics: Mozilla's Proxy Auto-Configuration PAC reference.
use crate::{NetConfig, ProxyChoice, ProxyMode, ProxyServer, SocksVersion};
use boa_engine::{
    js_string, module::IdleModuleLoader, Context, JsResult, JsString, JsValue, NativeFunction,
    Source,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{self, Read};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const MAX_SCRIPT: usize = 256 * 1024;
const MAX_RESULT: usize = 8192;
const MAX_ROUTES: usize = 16;
#[derive(Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    location: String,
    system: bool,
    automatic: bool,
    directory: Option<std::path::PathBuf>,
}
struct Cached {
    source: Arc<String>,
    fetched: Instant,
    generation: u64,
}
fn cache() -> &'static Mutex<HashMap<CacheKey, Cached>> {
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, Cached>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}
pub fn clear_cache() {
    cache().lock().unwrap_or_else(|e| e.into_inner()).clear();
}
fn source(cfg: &NetConfig) -> Result<Arc<String>, String> {
    let location = cfg.proxy.pac_url.trim();
    let normalized_scheme = location.to_ascii_lowercase();
    if location.is_empty() {
        return Err("Enter a PAC URL or local file".into());
    }
    let key = CacheKey {
        location: location.into(),
        system: cfg.certificates.use_system_store,
        automatic: cfg.certificates.accept_automatically,
        directory: cfg.certs_dir.clone(),
    };
    let generation = crate::GENERATION.load(Ordering::SeqCst);
    let age = Duration::from_secs(cfg.proxy.pac_refresh_seconds.clamp(30, 86400));
    if let Some(entry) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        if entry.generation == generation && entry.fetched.elapsed() < age {
            return Ok(entry.source.clone());
        }
    }
    let mut bytes = Vec::new();
    if normalized_scheme.starts_with("http://") || normalized_scheme.starts_with("https://") {
        let url = url::Url::parse(location).map_err(|_| "Invalid PAC URL")?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err("PAC URLs must not contain credentials".into());
        }
        let mut bootstrap = cfg.clone();
        bootstrap.proxy.mode = ProxyMode::NoProxy;
        let response = crate::agent_builder_with(&bootstrap, location)
            .timeout_connect(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .redirects(3)
            .user_agent(crate::USER_AGENT)
            .build()
            .get(location)
            .call()
            .map_err(|e| format!("Cannot load PAC: {e}"))?;
        response
            .into_reader()
            .take((MAX_SCRIPT + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
    } else {
        let path = if normalized_scheme.starts_with("file:") {
            url::Url::parse(location)
                .map_err(|_| "Invalid PAC file URL")?
                .to_file_path()
                .map_err(|_| "PAC file URL must identify a local file")?
        } else {
            if location.contains("://") {
                return Err("PAC supports HTTP, HTTPS and local files".into());
            }
            std::path::PathBuf::from(location)
        };
        std::fs::File::open(path)
            .map_err(|e| format!("Cannot open PAC: {e}"))?
            .take((MAX_SCRIPT + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
    }
    if bytes.len() > MAX_SCRIPT {
        return Err("PAC script exceeds 256 KiB".into());
    }
    let text = String::from_utf8(bytes).map_err(|_| "PAC must be UTF-8 JavaScript")?;
    let text = Arc::new(text.trim_start_matches('\u{feff}').to_owned());
    let mut entries = cache().lock().unwrap_or_else(|e| e.into_inner());
    if entries.len() >= 8 {
        if let Some(oldest) = entries
            .iter()
            .min_by_key(|(_, v)| v.fetched)
            .map(|(k, _)| k.clone())
        {
            entries.remove(&oldest);
        }
    }
    entries.insert(
        key,
        Cached {
            source: text.clone(),
            fetched: Instant::now(),
            generation,
        },
    );
    Ok(text)
}
struct DnsBudget {
    started: Instant,
    calls: usize,
    answers: HashMap<String, Option<String>>,
}
thread_local! { static DNS:RefCell<DnsBudget>=RefCell::new(DnsBudget {started:Instant::now(),calls:0,answers:HashMap::new()}); }
static DNS_WORKERS: AtomicUsize = AtomicUsize::new(0);
fn reserve_worker(counter: &AtomicUsize, limit: usize) -> bool {
    let mut current = counter.load(Ordering::SeqCst);
    loop {
        if current >= limit {
            return false;
        }
        match counter.compare_exchange_weak(
            current,
            current + 1,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
}
fn dns_lookup(host: String) -> Option<String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Some(ip.to_string());
    }
    let allowed = DNS.with(|state| {
        let mut state = state.borrow_mut();
        if let Some(v) = state.answers.get(&host) {
            return Some(v.clone());
        }
        if state.calls >= 16 || state.started.elapsed() > Duration::from_secs(3) {
            return Some(None);
        }
        state.calls += 1;
        None
    });
    if let Some(cached) = allowed {
        return cached;
    }
    if !reserve_worker(&DNS_WORKERS, 4) {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let lookup = host.clone();
    let spawned = std::thread::Builder::new()
        .name("rustpy-pac-dns".into())
        .spawn(move || {
            struct Permit;
            impl Drop for Permit {
                fn drop(&mut self) {
                    DNS_WORKERS.fetch_sub(1, Ordering::SeqCst);
                }
            }
            let _permit = Permit;
            let answer = (lookup.as_str(), 0)
                .to_socket_addrs()
                .ok()
                .and_then(|addresses| {
                    let ips: Vec<_> = addresses.map(|a| a.ip()).collect();
                    ips.iter()
                        .find(|ip| ip.is_ipv4())
                        .or(ips.first())
                        .map(ToString::to_string)
                });
            let _ = tx.send(answer);
        });
    if spawned.is_err() {
        DNS_WORKERS.fetch_sub(1, Ordering::SeqCst);
        return None;
    }
    let answer = rx.recv_timeout(Duration::from_millis(750)).ok().flatten();
    DNS.with(|state| {
        state.borrow_mut().answers.insert(host, answer.clone());
    });
    answer
}
fn dns_native(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let host = args
        .first()
        .cloned()
        .unwrap_or_default()
        .to_string(context)?
        .to_std_string_escaped();
    if host.len() > 253 || host.contains(['\r', '\n', '\0']) {
        return Ok(JsValue::null());
    }
    Ok(dns_lookup(host)
        .map(|ip| JsValue::from(JsString::from(ip.as_str())))
        .unwrap_or_else(JsValue::null))
}
fn local_ip(_this: &JsValue, _args: &[JsValue], _context: &mut Context) -> JsResult<JsValue> {
    let ip = UdpSocket::bind("0.0.0.0:0")
        .and_then(|socket| {
            socket.connect("192.0.2.1:9")?;
            socket.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".into());
    Ok(JsValue::from(JsString::from(ip.as_str())))
}
/// No filesystem, process, HTTP or module-loader globals are exposed. DNS has a
/// per-evaluation time/query budget and a process-wide cap on outstanding lookups.
pub fn evaluate(script: &str, target: &str) -> Result<String, String> {
    if script.len() > MAX_SCRIPT || target.len() > 8192 {
        return Err("PAC input exceeds its size limit".into());
    }
    let mut url = url::Url::parse(target).map_err(|_| "Invalid PAC target URL")?;
    let host = url.host_str().ok_or("PAC target has no host")?.to_owned();
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_fragment(None);
    if matches!(url.scheme(), "https" | "wss") {
        url.set_path("/");
        url.set_query(None);
    }
    DNS.with(|state| {
        *state.borrow_mut() = DnsBudget {
            started: Instant::now(),
            calls: 0,
            answers: HashMap::new(),
        }
    });
    // Do not inherit Boa's default filesystem module loader or permit
    // Atomics.wait to suspend this synchronous PAC evaluation thread.
    let mut context = Context::builder()
        .module_loader(Rc::new(IdleModuleLoader))
        .can_block(false)
        .build()
        .map_err(|error| format!("PAC context: {error}"))?;
    context.runtime_limits_mut().set_loop_iteration_limit(10000);
    context.runtime_limits_mut().set_recursion_limit(64);
    context.runtime_limits_mut().set_stack_size_limit(1024);
    context
        .register_global_builtin_callable(
            js_string!("dnsResolve"),
            1,
            NativeFunction::from_fn_ptr(dns_native),
        )
        .map_err(|e| e.to_string())?;
    context
        .register_global_builtin_callable(
            js_string!("myIpAddress"),
            0,
            NativeFunction::from_fn_ptr(local_ip),
        )
        .map_err(|e| e.to_string())?;
    context
        .eval(Source::from_bytes(include_str!("pac_helpers.js")))
        .map_err(|e| format!("PAC helpers: {e}"))?;
    context
        .eval(Source::from_bytes(script))
        .map_err(|e| format!("PAC script: {e}"))?;
    let call = format!(
        "FindProxyForURL({}, {})",
        serde_json::to_string(url.as_str()).map_err(|e| e.to_string())?,
        serde_json::to_string(&host).map_err(|e| e.to_string())?
    );
    let value = context
        .eval(Source::from_bytes(&call))
        .map_err(|e| format!("FindProxyForURL: {e}"))?;
    let result = value
        .as_string()
        .ok_or("FindProxyForURL must return a string")?
        .to_std_string_escaped();
    if result.len() > MAX_RESULT {
        return Err("PAC result exceeds 8 KiB".into());
    }
    Ok(result)
}
pub fn parse_result(
    result: &str,
    settings: &crate::ProxySettings,
) -> Result<Vec<ProxyChoice>, String> {
    if result.len() > MAX_RESULT {
        return Err("PAC result exceeds 8 KiB".into());
    }
    let mut routes = Vec::new();
    for directive in result.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        if routes.len() >= MAX_ROUTES {
            return Err("PAC returned more than 16 routes".into());
        }
        let parts: Vec<_> = directive.split_whitespace().collect();
        if parts.len() == 1 && parts[0].eq_ignore_ascii_case("DIRECT") {
            routes.push(ProxyChoice::Direct);
            continue;
        }
        if parts.len() != 2 {
            return Err("Invalid PAC proxy directive".into());
        }
        let parsed = url::Url::parse(&format!("http://{}", parts[1]))
            .map_err(|_| "Invalid PAC proxy address")?;
        if !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.path() != "/"
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err("PAC proxy address must be host:port without credentials".into());
        }
        let port = parts[1]
            .rsplit_once(':')
            .and_then(|(_, p)| p.parse::<u16>().ok())
            .filter(|p| *p != 0)
            .ok_or("PAC proxy must include a port")?;
        let host = parsed
            .host_str()
            .ok_or("PAC proxy has no host")?
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        let credentials = settings.credentials();
        let server = ProxyServer {
            host,
            port,
            user: credentials.map(|(u, _)| u.to_owned()),
            password: credentials.map(|(_, p)| p.to_owned()),
        };
        routes.push(match parts[0].to_ascii_uppercase().as_str() {
            "PROXY" | "HTTP" => ProxyChoice::Http(server),
            "SOCKS" => ProxyChoice::Socks(server, SocksVersion::V4a),
            "SOCKS4" => ProxyChoice::Socks(server, SocksVersion::V4),
            "SOCKS5" => ProxyChoice::Socks(server, SocksVersion::V5),
            _ => {
                return Err(
                    "Unsupported PAC proxy protocol (use PROXY, SOCKS, SOCKS4, SOCKS5 or DIRECT)"
                        .into(),
                )
            }
        });
    }
    if routes.is_empty() {
        return Err("PAC returned no routes".into());
    }
    Ok(routes)
}
pub fn choose_with(cfg: &NetConfig, target: &str) -> ProxyChoice {
    if let Some((_, host, port)) = crate::proxy::url_target(target) {
        if crate::proxy::is_loopback(&host)
            || crate::proxy::no_proxy_matches(&cfg.proxy.no_proxy, &host, port)
        {
            return ProxyChoice::Direct;
        }
    }
    match source(cfg)
        .and_then(|script| evaluate(&script, target))
        .and_then(|result| parse_result(&result, &cfg.proxy))
    {
        Ok(routes) => ProxyChoice::Pac(routes),
        Err(error) => ProxyChoice::Unavailable(format!("PAC: {error}")),
    }
}
/// PAC never falls back to direct unless the script explicitly returned DIRECT.
/// Failover happens on transport connection errors, not on HTTP origin statuses.
pub struct PacResolver {
    pub config: Arc<NetConfig>,
}
static BRIDGES: AtomicUsize = AtomicUsize::new(0);
impl ureq::Resolver for PacResolver {
    fn resolve(&self, netloc: &str) -> io::Result<Vec<SocketAddr>> {
        let (host, port) = crate::socks::split_netloc(netloc)?;
        if !reserve_worker(&BRIDGES, 32) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Too many active PAC connections",
            ));
        }
        struct Permit;
        impl Drop for Permit {
            fn drop(&mut self) {
                BRIDGES.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let permit = Permit;
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let cfg = self.config.clone();
        std::thread::Builder::new()
            .name("rustpy-pac-connection".into())
            .spawn(move || {
                let _permit = permit;
                let Some(client) = crate::socks::accept_one(listener) else {
                    return;
                };
                if let Err(error) = serve(client, &cfg, &host, port) {
                    log::warn!("PAC connection: {error}");
                }
            })?;
        Ok(vec![address])
    }
}
fn serve(
    mut client: std::net::TcpStream,
    cfg: &NetConfig,
    host: &str,
    port: u16,
) -> io::Result<()> {
    use std::io::Write;
    const TIMEOUT: Duration = Duration::from_secs(15);
    client.set_read_timeout(Some(TIMEOUT))?;
    let mut first = [0u8; 1];
    if client.peek(&mut first)? == 0 {
        return Ok(());
    }
    let tls = first[0] == 0x16;
    let authority = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let (head, rest, target) = if tls {
        (Vec::new(), Vec::new(), format!("https://{authority}/"))
    } else {
        let (head, rest) = crate::http_bridge::read_head(&mut client)?;
        let line = std::str::from_utf8(head.split(|b| *b == b'\n').next().unwrap_or_default())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid HTTP request"))?;
        let path = line
            .split_whitespace()
            .nth(1)
            .filter(|p| p.starts_with('/'))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "PAC accepts only origin-form requests",
                )
            })?;
        let target = format!("http://{authority}{path}");
        (head, rest, target)
    };
    // Evaluate for the actual HTTP request, not just the URL used when the agent
    // was constructed. HTTPS has host-only PAC semantics. Redirects and shared
    // agents therefore reevaluate the destination instead of inheriting a route.
    let routes = match choose_with(cfg, &target) {
        ProxyChoice::Pac(routes) => routes,
        ProxyChoice::Direct => vec![ProxyChoice::Direct],
        ProxyChoice::Unavailable(error) => {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, error))
        }
        other => vec![other],
    };
    let deadline = Instant::now() + TIMEOUT;
    let mut last = io::Error::new(io::ErrorKind::NotConnected, "No PAC route connected");
    for route in routes {
        let timeout = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(5));
        if timeout.is_zero() {
            break;
        }
        let result = match &route {
            ProxyChoice::Direct => crate::socks::connect_proxy(
                &ProxyServer {
                    host: host.into(),
                    port,
                    user: None,
                    password: None,
                },
                timeout,
            ),
            ProxyChoice::Http(proxy) => {
                crate::socks::connect_proxy(proxy, timeout).and_then(|mut stream| {
                    stream.set_read_timeout(Some(timeout))?;
                    stream.set_write_timeout(Some(timeout))?;
                    if tls {
                        crate::http_bridge::connect_tunnel(&mut stream, proxy, host, port)?;
                    }
                    Ok(stream)
                })
            }
            ProxyChoice::Socks(proxy, version) => {
                crate::socks::connect(proxy, *version, host, port, timeout)
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid nested PAC route",
            )),
        };
        let mut upstream = match result {
            Ok(upstream) => upstream,
            Err(error) => {
                // Authentication refusals do not silently bypass the policy with a
                // later DIRECT result; an unavailable proxy can use explicit fallback.
                if error.kind() == io::ErrorKind::PermissionDenied {
                    return Err(error);
                }
                last = error;
                continue;
            }
        };
        upstream.set_read_timeout(None)?;
        upstream.set_write_timeout(None)?;
        client.set_read_timeout(None)?;
        if tls {
            crate::socks::relay(client, upstream);
            return Ok(());
        }
        let request = match &route {
            ProxyChoice::Http(proxy) => crate::http_bridge::rewrite_request_head(
                &head, host, port, proxy,
            )
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid proxied request"))?,
            _ => origin_head(&head)?,
        };
        upstream.write_all(&request)?;
        upstream.write_all(&rest)?;
        return crate::http_bridge::relay_one_request(client, upstream);
    }
    Err(last)
}
fn origin_head(head: &[u8]) -> io::Result<Vec<u8>> {
    let text = std::str::from_utf8(head)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid request head"))?;
    let mut lines = text.split("\r\n");
    let first = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Missing request line"))?;
    let mut out = format!("{first}\r\n");
    for line in lines.filter(|line| !line.is_empty()) {
        let name = line.split(':').next().unwrap_or_default().trim();
        if [
            "connection",
            "proxy-connection",
            "proxy-authorization",
            "keep-alive",
        ]
        .iter()
        .any(|n| name.eq_ignore_ascii_case(n))
        {
            continue;
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out.push_str("Connection: close\r\n\r\n");
    Ok(out.into_bytes())
}
pub struct UnavailableResolver(pub String);
impl ureq::Resolver for UnavailableResolver {
    fn resolve(&self, _netloc: &str) -> io::Result<Vec<SocketAddr>> {
        Err(io::Error::new(io::ErrorKind::InvalidInput, self.0.clone()))
    }
}

#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn javascript_routes_real_host_and_path_and_strips_https_secrets() {
        let script = r#"function FindProxyForURL(url,host) {
            if (dnsDomainIs(host,'.corp.test') && shExpMatch(url,'http://*/private/*')) return 'PROXY gateway.test:80; SOCKS5 socks.test:1080; DIRECT';
            if (url.indexOf('secret')>=0 || url.indexOf('password')>=0) throw Error('leaked URL information');
            return 'DIRECT';
        }"#;
        let result = evaluate(script, "http://service.corp.test/private/report").unwrap();
        let routes = parse_result(&result, &crate::ProxySettings::default()).unwrap();
        assert_eq!(routes.len(), 3);
        assert!(
            matches!(&routes[0],ProxyChoice::Http(proxy) if proxy.host == "gateway.test" && proxy.port == 80)
        );
        assert_eq!(
            evaluate(
                script,
                "https://user:password@service.corp.test/secret?token=secret"
            )
            .unwrap(),
            "DIRECT"
        );
    }
    #[test]
    fn dns_literal_globs_and_complete_calendar_helpers_do_not_need_external_services() {
        let script = r#"const OriginalDate=Date; Date=function(){return new OriginalDate('2026-10-15T12:30:45Z');};
            function FindProxyForURL() {
                if (!isInNet('192.0.2.7','192.0.2.0','255.255.255.0')) throw Error('subnet');
                if (dnsResolve('192.0.2.7')!=='192.0.2.7') throw Error('literal DNS');
                if (!shExpMatch('a.b[1]','a.b[?]') || shExpMatch('axb[1]','a.b[?]')) throw Error('glob escaping');
                if (!localHostOrDomainIs('server','server.corp.test') || dnsDomainLevels('server.corp.test')!==2) throw Error('host helpers');
                if (!weekdayRange('THU','THU','GMT') || !timeRange(12,30,12,31,'GMT')) throw Error('time');
                if (!dateRange(15,'OCT',2026,15,'OCT',2026,'GMT')) throw Error('date');
                return 'DIRECT';
            }"#;
        assert_eq!(evaluate(script, "http://calendar.test/").unwrap(), "DIRECT");
    }
    #[test]
    fn javascript_engine_upgrade_keeps_pac_capabilities_and_result_contract() {
        let script = r#"function FindProxyForURL() {
            if (typeof process !== 'undefined' || typeof require !== 'undefined' ||
                typeof fetch !== 'undefined' || typeof readFile !== 'undefined')
                throw Error('unexpected host capability');
            return 'DIRECT';
        }"#;
        assert_eq!(evaluate(script, "https://sandbox.test/").unwrap(), "DIRECT");
        // PAC is synchronous: promise results do not run queued module/jobs or
        // get coerced into a valid proxy route by the embedding layer.
        assert!(evaluate(
            "async function FindProxyForURL(){ return 'DIRECT'; }",
            "https://sandbox.test/"
        )
        .unwrap_err()
        .contains("must return a string"));
        assert!(evaluate(
            "function FindProxyForURL(){ return 'x'.repeat(8193); }",
            "https://sandbox.test/"
        )
        .unwrap_err()
        .contains("exceeds 8 KiB"));
        assert!(evaluate(
            "function FindProxyForURL(){ return FindProxyForURL(); }",
            "https://sandbox.test/"
        )
        .is_err());
    }
    #[test]
    fn malformed_and_unbounded_scripts_fail_without_implicit_direct() {
        assert!(evaluate(
            "function FindProxyForURL(){while(true){}}",
            "https://limit.test/"
        )
        .is_err());
        assert!(evaluate(
            "function FindProxyForURL(){return 42}",
            "https://limit.test/"
        )
        .is_err());
        assert!(parse_result(
            "PROXY user:password@proxy.test:8080",
            &crate::ProxySettings::default()
        )
        .is_err());
        assert!(parse_result("INVALID proxy.test:80", &crate::ProxySettings::default()).is_err());
        assert!(parse_result("", &crate::ProxySettings::default()).is_err());
        assert!(evaluate(&"x".repeat(MAX_SCRIPT + 1), "https://limit.test/").is_err());
        let routes = parse_result(
            "SOCKS5 [2001:db8::1]:1080; DIRECT",
            &crate::ProxySettings::default(),
        )
        .unwrap();
        assert!(
            matches!(&routes[0],ProxyChoice::Socks(proxy,SocksVersion::V5) if proxy.host == "2001:db8::1")
        );
    }
    #[test]
    fn credentials_are_only_proxy_transport_data_and_origin_headers_are_scrubbed() {
        let settings = crate::ProxySettings {
            authenticate: true,
            login: "user".into(),
            password: "private-password".into(),
            ..Default::default()
        };
        let routes = parse_result("PROXY gateway.test:8080; DIRECT", &settings).unwrap();
        assert!(!format!("{routes:?}").contains("private-password"));
        assert!(!format!("{settings:?}").contains("private-password"));
        assert!(
            matches!(&routes[0],ProxyChoice::Http(proxy) if proxy.password.as_deref()==Some("private-password"))
        );
        let head=origin_head(b"GET /path HTTP/1.1\r\nHost: origin.test\r\nProxy-Authorization: Basic private\r\nConnection: keep-alive\r\n\r\n").unwrap();
        let head = String::from_utf8(head).unwrap();
        assert!(!head.contains("private"));
        assert!(!head.contains("keep-alive"));
        assert!(head.ends_with("Connection: close\r\n\r\n"));
    }
}

#[cfg(test)]
mod automatic_proxy_fixtures {
    use super::*;
    #[test]
    fn invalid_environment_proxy_cannot_bypass_policy_or_downgrade_tls_credentials() {
        let settings = crate::ProxySettings::default();
        let env = |name: &str| {
            (name == "HTTPS_PROXY").then(|| "https://user:private-password@gateway.test:443".into())
        };
        let route = crate::proxy::choose(&settings, "https://origin.test/", &env);
        assert!(matches!(&route, ProxyChoice::Unavailable(_)));
        assert!(!format!("{route:?}").contains("private-password"));
        assert!(
            crate::proxy::parse_proxy_url("https://user:private-password@gateway.test:443")
                .is_err()
        );
    }
}

#[cfg(test)]
mod transport_fixtures {
    use super::*;
    use std::io::Write;
    fn proxy(response: String) -> (u16, std::thread::JoinHandle<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let worker = std::thread::spawn(move || {
            let started = Instant::now();
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        // Accepted sockets inherit the listener's non-blocking mode on
                        // BSD systems and Windows.
                        stream.set_nonblocking(false).unwrap();
                        break stream;
                    }
                    Err(error)
                        if error.kind() == io::ErrorKind::WouldBlock
                            && started.elapsed() < Duration::from_secs(5) =>
                    {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("PAC fixture proxy: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let (head, _) = crate::http_bridge::read_head(&mut stream).unwrap();
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8(head).unwrap()
        });
        (port, worker)
    }
    #[test]
    fn reused_agent_redirect_reevaluates_pac_host_and_http_path_with_proxy_only_credentials() {
        let _guard = crate::test_lock();
        let (second, second_request) =
            proxy("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".into());
        let (first,first_request)=proxy("HTTP/1.1 302 Found\r\nLocation: http://second.test/private/report\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into());
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("proxy.pac");
        std::fs::write(&file,format!("function FindProxyForURL(url,host) {{ if(host==='first.test') return 'PROXY 127.0.0.1:{first}'; if(host==='second.test' && shExpMatch(url,'http://*/private/*')) return 'PROXY 127.0.0.1:{second}'; throw Error('Wrong PAC destination'); }}")).unwrap();
        let cfg = NetConfig {
            proxy: crate::ProxySettings {
                mode: ProxyMode::Pac,
                pac_url: file.display().to_string(),
                authenticate: true,
                login: "fixture-user".into(),
                password: "fixture-password".into(),
                ..Default::default()
            },
            certificates: crate::CertificateSettings {
                use_system_store: false,
                ..Default::default()
            },
            certs_dir: None,
        };
        let response = crate::agent_builder_with(&cfg, "http://first.test/begin")
            .timeout(Duration::from_secs(10))
            .build()
            .get("http://first.test/begin")
            .call()
            .unwrap();
        assert_eq!(response.into_string().unwrap(), "ok");
        let first = first_request.join().unwrap();
        let second = second_request.join().unwrap();
        assert!(first.starts_with("GET http://first.test/begin HTTP/1.1"));
        assert!(second.starts_with("GET http://second.test/private/report HTTP/1.1"));
        for head in [first, second] {
            assert!(head.contains("Proxy-Authorization: Basic "));
            assert!(!head.contains("fixture-password"));
            assert!(head.contains("Connection: close"));
        }
        clear_cache();
    }
}
