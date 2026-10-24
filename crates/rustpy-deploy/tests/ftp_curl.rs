//! [`FtpTransport`] end to end with the real `curl` client against a minimal FTP server that
//! runs inside this test process on 127.0.0.1 (loopback only; no other host is contacted).
//! The server implements just what curl uses: login, `PWD`/`CWD`/`MKD`/`RMD`/`DELE`,
//! `EPSV`/`PASV`, `STOR`/`RETR`/`SIZE`/`MDTM`/`MFMT`, `LIST` and (optionally) `MLSD`.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::{mtime, read, set_mtime, write, TempDir};
use rustpy_deploy::{FtpTarget, FtpTransport, SyncDirection, Transport};

const T: i64 = 1_700_000_000;
const USER: &str = "bob";
/// Exercises the curl config escaping (quote, backslash) and the `user:password` split.
const PASSWORD: &str = "p\"w\\d:x y";

fn installed(program: &str) -> bool {
    match Command::new(program).arg("-V").output() {
        Ok(_) => true,
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

// ---------------------------------------------------------------------------
// Civil time (UTC)
// ---------------------------------------------------------------------------

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `YYYYMMDDHHMMSS`
fn ftp_time(t: i64) -> String {
    let (y, m, d) = civil_from_days(t.div_euclid(86_400));
    let s = t.rem_euclid(86_400);
    format!(
        "{y:04}{m:02}{d:02}{:02}{:02}{:02}",
        s / 3600,
        s % 3600 / 60,
        s % 60
    )
}

fn parse_ftp_time(s: &str) -> Option<i64> {
    if s.len() != 14 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s[r].parse::<i64>().unwrap();
    Some(
        days_from_civil(n(0..4), n(4..6), n(6..8)) * 86_400
            + n(8..10) * 3600
            + n(10..12) * 60
            + n(12..14),
    )
}

/// The date column of `ls -l`: `Nov 14 22:13` for recent files, `Nov 14  2023` otherwise.
fn ls_time(t: i64, now: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (y, m, d) = civil_from_days(t.div_euclid(86_400));
    let month = MONTHS[(m - 1) as usize];
    if (now - t).abs() < 180 * 86_400 {
        let s = t.rem_euclid(86_400);
        format!("{month} {d:>2} {:02}:{:02}", s / 3600, s % 3600 / 60)
    } else {
        format!("{month} {d:>2}  {y}")
    }
}

fn secs(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

// ---------------------------------------------------------------------------
// The server
// ---------------------------------------------------------------------------

struct Config {
    root: PathBuf,
    mlsd: bool,
    log: Mutex<Vec<String>>,
}

struct FtpServer {
    port: u16,
    config: Arc<Config>,
}

impl FtpServer {
    /// Serves `root` (`/` of the FTP namespace; logins start in `/home/deploy`).
    fn start(root: &Path, mlsd: bool) -> FtpServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let config = Arc::new(Config {
            root: root.to_path_buf(),
            mlsd,
            log: Mutex::new(Vec::new()),
        });
        let shared = config.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let config = shared.clone();
                std::thread::spawn(move || {
                    let _ = session(stream, &config);
                });
            }
        });
        FtpServer { port, config }
    }

    /// Every command received so far.
    fn commands(&self) -> Vec<String> {
        self.config.log.lock().unwrap().clone()
    }
}

/// Resolves an FTP path argument against the working directory (both `/`-rooted).
fn resolve(cwd: &str, arg: &str) -> String {
    let mut parts: Vec<&str> = if arg.starts_with('/') {
        Vec::new()
    } else {
        cwd.split('/').filter(|s| !s.is_empty()).collect()
    };
    for seg in arg.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    format!("/{}", parts.join("/"))
}

fn real(config: &Config, path: &str) -> PathBuf {
    let mut p = config.root.clone();
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        p.push(seg);
    }
    p
}

fn send(out: &mut TcpStream, text: &str) -> std::io::Result<()> {
    out.write_all(format!("{text}\r\n").as_bytes())
}

fn listing(dir: &Path, mlsd: bool) -> String {
    let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let mut out = if mlsd {
        format!("type=cdir;modify={}; .\r\n", ftp_time(now))
    } else {
        String::new()
    };
    for e in entries {
        let meta = e.metadata().unwrap();
        let name = e.file_name().to_string_lossy().into_owned();
        let t = secs(&meta);
        out += &match (mlsd, meta.is_dir()) {
            (true, true) => format!("type=dir;modify={};perm=flcdmpe; {name}\r\n", ftp_time(t)),
            (true, false) => format!(
                "type=file;size={};modify={};UNIX.mode=0644; {name}\r\n",
                meta.len(),
                ftp_time(t)
            ),
            (false, is_dir) => format!(
                "{} 1 ftp ftp {:>10} {} {name}\r\n",
                if is_dir { "drwxr-xr-x" } else { "-rw-r--r--" },
                meta.len(),
                ls_time(t, now)
            ),
        };
    }
    out
}

fn session(stream: TcpStream, config: &Config) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut out = stream;
    send(&mut out, "220 rustpy-deploy test server")?;
    let (mut user, mut logged_in, mut cwd) = (String::new(), false, "/home/deploy".to_string());
    let mut passive: Option<TcpListener> = None;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let text = line.trim_end_matches(['\r', '\n']).to_string();
        config.log.lock().unwrap().push(text.clone());
        let (cmd, arg) = match text.split_once(' ') {
            Some((c, a)) => (c.to_ascii_uppercase(), a.to_string()),
            None => (text.to_ascii_uppercase(), String::new()),
        };
        let path = resolve(&cwd, &arg);
        let file = real(config, &path);
        match cmd.as_str() {
            "USER" => {
                user = arg;
                send(&mut out, "331 Password required")?;
            }
            "PASS" => {
                logged_in = user == USER && arg == PASSWORD;
                send(
                    &mut out,
                    if logged_in {
                        "230 Logged in"
                    } else {
                        "530 Login incorrect"
                    },
                )?;
            }
            "QUIT" => {
                send(&mut out, "221 Goodbye")?;
                return Ok(());
            }
            _ if !logged_in => send(&mut out, "530 Please log in")?,
            "PWD" => send(&mut out, &format!("257 \"{cwd}\" is the current directory"))?,
            "CWD" if file.is_dir() => {
                cwd = path;
                send(&mut out, "250 OK")?;
            }
            "CWD" => send(&mut out, "550 No such directory")?,
            "MKD" => match std::fs::create_dir(&file) {
                Ok(()) => send(&mut out, &format!("257 \"{path}\" created"))?,
                Err(_) => send(&mut out, "550 Cannot create directory")?,
            },
            "RMD" => match std::fs::remove_dir(&file) {
                Ok(()) => send(&mut out, "250 Removed")?,
                Err(_) => send(&mut out, "550 Cannot remove directory")?,
            },
            "DELE" if file.is_file() => {
                std::fs::remove_file(&file)?;
                send(&mut out, "250 Deleted")?;
            }
            "DELE" => send(&mut out, "550 Not a file")?,
            "TYPE" | "OPTS" | "NOOP" => send(&mut out, "200 OK")?,
            "SYST" => send(&mut out, "215 UNIX Type: L8")?,
            "EPSV" | "PASV" => {
                let data = TcpListener::bind("127.0.0.1:0")?;
                let port = data.local_addr()?.port();
                passive = Some(data);
                if cmd == "EPSV" {
                    send(
                        &mut out,
                        &format!("229 Entering Extended Passive Mode (|||{port}|)"),
                    )?;
                } else {
                    send(
                        &mut out,
                        &format!(
                            "227 Entering Passive Mode (127,0,0,1,{},{})",
                            port / 256,
                            port % 256
                        ),
                    )?;
                }
            }
            "SIZE" | "MDTM" if file.is_file() => {
                let meta = std::fs::metadata(&file)?;
                send(
                    &mut out,
                    &format!(
                        "213 {}",
                        if cmd == "SIZE" {
                            meta.len().to_string()
                        } else {
                            ftp_time(secs(&meta))
                        }
                    ),
                )?;
            }
            "SIZE" | "MDTM" => send(&mut out, "550 No such file")?,
            "MFMT" => {
                let target = arg.split_once(' ').and_then(|(ts, name)| {
                    Some((parse_ftp_time(ts)?, real(config, &resolve(&cwd, name))))
                });
                match target {
                    Some((t, p)) if p.is_file() => {
                        set_mtime(&p, t);
                        send(&mut out, &format!("213 Modify={}; {arg}", ftp_time(t)))?;
                    }
                    _ => send(&mut out, "550 Cannot set the time")?,
                }
            }
            "STOR" | "RETR" | "LIST" | "MLSD" => {
                let Some(data) = passive.take() else {
                    send(&mut out, "425 Use EPSV or PASV first")?;
                    continue;
                };
                let listed = if arg.is_empty() {
                    real(config, &cwd)
                } else {
                    file.clone()
                };
                let refusal = match cmd.as_str() {
                    "MLSD" if !config.mlsd => Some("500 Unknown command"),
                    "RETR" if !file.is_file() => Some("550 No such file"),
                    "LIST" | "MLSD" if !listed.is_dir() => Some("550 No such directory"),
                    _ => None,
                };
                if let Some(refusal) = refusal {
                    send(&mut out, refusal)?;
                    continue;
                }
                send(&mut out, "150 Opening data connection")?;
                let (mut conn, _) = data.accept()?;
                match cmd.as_str() {
                    "STOR" => {
                        let mut bytes = Vec::new();
                        conn.read_to_end(&mut bytes)?;
                        std::fs::write(&file, bytes)?;
                    }
                    "RETR" => conn.write_all(&std::fs::read(&file)?)?,
                    _ => conn.write_all(listing(&listed, cmd == "MLSD").as_bytes())?,
                }
                drop(conn);
                send(&mut out, "226 Transfer complete")?;
            }
            _ => send(&mut out, "502 Command not implemented")?,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn ftp_transport_against_a_loopback_server() {
    if !installed("curl") {
        eprintln!("skipped: curl is not installed");
        return;
    }
    let tmp = TempDir::new("ftp");
    let fsroot = tmp.path().join("ftproot");
    std::fs::create_dir_all(fsroot.join("home/deploy")).unwrap();
    let server = FtpServer::start(&fsroot, true);
    let t = FtpTransport::new(
        FtpTarget::new("127.0.0.1", server.port, USER, Some(PASSWORD.into())),
        "/srv/my site",
    )
    .unwrap()
    .with_timeouts(Duration::from_secs(30), Duration::from_secs(60));
    let root = fsroot.join("srv/my site");

    // The root path does not exist yet; mkdirs creates it with all parents.
    let err = t.test_connection().unwrap_err();
    assert!(format!("{err:#}").contains("FTP connection to"), "{err:#}");
    t.mkdirs("/srv/my site/pkg/[x] dir").unwrap();
    assert!(root.join("pkg/[x] dir").is_dir());
    assert!(t.test_connection().unwrap().contains("has 1 entries"));

    // Uploads with URL-special names keep the modification time (MFMT); missing directories
    // are created; relative paths start at the login directory.
    let names = [
        "a b.py",
        "it's \"q\" $HOME.py",
        "x[1].py",
        "star*?.txt",
        "semi;type=a.txt",
        "pct%41 #1.txt",
        "é ü.py",
    ];
    let src = tmp.path().join("src");
    for (i, name) in names.iter().enumerate() {
        let local = src.join(name);
        write(&local, &format!("content of {name}"), T + i as i64);
        t.upload(&local, &format!("/srv/my site/{name}"))
            .unwrap_or_else(|e| panic!("upload {name}: {e:#}"));
        assert_eq!(read(&root.join(name)), format!("content of {name}"));
        assert_eq!(mtime(&root.join(name)), T + i as i64, "mtime of {name}");
    }
    let deep = src.join("deep.py");
    write(&deep, "deep", T);
    t.upload(&deep, "/srv/my site/new dir/sub/deep.py").unwrap();
    assert_eq!(read(&root.join("new dir/sub/deep.py")), "deep");
    t.upload(&deep, "rel dir/deep.py").unwrap();
    assert_eq!(read(&fsroot.join("home/deploy/rel dir/deep.py")), "deep");

    // MLSD listings carry exact sizes and UTC times.
    let entries = t.list("/srv/my site").unwrap();
    let got: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    let mut expected: Vec<&str> = names.iter().copied().chain(["new dir", "pkg"]).collect();
    expected.sort();
    assert_eq!(got, expected);
    let a = entries.iter().find(|e| e.name == "a b.py").unwrap();
    assert_eq!(
        (a.is_dir, a.size, a.mtime),
        (false, "content of a b.py".len() as u64, Some(T))
    );
    let all = t
        .list_recursive("/srv/my site", &["*.txt".to_string()])
        .unwrap();
    assert!(
        all.iter().any(|e| e.name == "new dir/sub/deep.py"),
        "{all:?}"
    );
    assert!(all.iter().any(|e| e.name == "pkg/[x] dir" && e.is_dir));
    assert!(!all.iter().any(|e| e.name.ends_with(".txt")));
    assert!(
        t.list_recursive("/srv/missing", &[]).unwrap().is_empty(),
        "a missing directory lists as empty"
    );

    // Downloads get the server's modification time; a failed one leaves nothing behind.
    for (i, name) in names.iter().enumerate() {
        let local = tmp.path().join("down").join(name);
        t.download(&format!("/srv/my site/{name}"), &local)
            .unwrap_or_else(|e| panic!("download {name}: {e:#}"));
        assert_eq!(read(&local), format!("content of {name}"));
        assert_eq!(mtime(&local), T + i as i64, "mtime of downloaded {name}");
    }
    let missing = tmp.path().join("down/missing.py");
    assert!(t.download("/srv/my site/missing.py", &missing).is_err());
    assert!(!missing.exists());

    // Deleting a file and a whole tree (DELE fails on a directory, so it is emptied and RMD'd).
    t.delete("/srv/my site/a b.py").unwrap();
    assert!(!root.join("a b.py").exists());
    t.delete("/srv/my site/new dir").unwrap();
    assert!(!root.join("new dir").exists());
    assert!(root.join("pkg/[x] dir").is_dir(), "siblings stay");
    t.delete("/srv/my site/never existed.py").unwrap();
    assert!(t.delete("/").is_err());

    // curl delivered the password exactly (it travels in a config on stdin, not in argv).
    let commands = server.commands();
    assert!(
        commands.contains(&format!("PASS {PASSWORD}")),
        "{commands:?}"
    );
    assert!(commands.iter().any(|c| c.starts_with("MFMT ")));
    assert!(commands.iter().any(|c| c == "MLSD"));

    let wrong = FtpTransport::new(
        FtpTarget::new("127.0.0.1", server.port, USER, Some("wrong".into())),
        "/",
    )
    .unwrap();
    let err = format!("{:#}", wrong.list("/").unwrap_err());
    assert!(err.contains("Login denied"), "{err}");
}

#[test]
fn ftp_listing_falls_back_to_list_without_mlsd() {
    if !installed("curl") {
        eprintln!("skipped: curl is not installed");
        return;
    }
    let tmp = TempDir::new("ftp-list");
    let fsroot = tmp.path().join("ftproot");
    write(&fsroot.join("home/deploy/www/a b.py"), "12345", T);
    std::fs::create_dir_all(fsroot.join("home/deploy/www/sub dir")).unwrap();
    let server = FtpServer::start(&fsroot, false);
    let t = FtpTransport::new(
        FtpTarget::new("127.0.0.1", server.port, USER, Some(PASSWORD.into())),
        "www",
    )
    .unwrap();

    let entries = t.list("www").unwrap();
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_eq!(
        (entries[0].name.as_str(), entries[0].is_dir, entries[0].size),
        ("a b.py", false, 5)
    );
    assert_eq!(
        entries[0].mtime,
        Some(T - T.rem_euclid(86_400)),
        "LIST shows only the day of old files"
    );
    assert_eq!(
        (entries[1].name.as_str(), entries[1].is_dir),
        ("sub dir", true)
    );
    t.list("www").unwrap();
    let mlsd = server.commands().iter().filter(|c| *c == "MLSD").count();
    assert_eq!(mlsd, 1, "after one failure LIST is used directly");

    // Coarse LIST times still compare equal to the local file.
    let local = tmp.path().join("local");
    write(&local.join("a b.py"), "12345", T);
    std::fs::create_dir_all(local.join("sub dir")).unwrap();
    let opts = rustpy_deploy::SyncOptions {
        coarse_remote_times: t.coarse_times(),
        ..rustpy_deploy::SyncOptions::default()
    };
    let plan = rustpy_deploy::plan_sync_with(
        &local,
        &[],
        &t.list_recursive("www", &[]).unwrap(),
        SyncDirection::Both,
        &opts,
    );
    assert!(plan.is_empty(), "{plan:?}");
}
