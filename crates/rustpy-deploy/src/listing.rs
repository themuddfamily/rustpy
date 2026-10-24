//! Parsers for remote directory listings:
//!
//! * GNU `find -printf '%Y %s %T@ %f\0'` (or `\n`-terminated) records,
//! * `ls -l` style lines (`ls -la`, `ls -la --time-style=+%s`, `--time-style=long-iso`, and
//!   the Unix-style FTP `LIST` output),
//! * MS-DOS / IIS style FTP `LIST` lines,
//! * FTP `MLSD` facts.
//!
//! Timestamps are Unix seconds (UTC). `ls`/`LIST` dates without a year (`Jan 15 10:30`) are
//! placed in the year that makes them not lie in the future relative to `now`.

use crate::RemoteEntry;

// ---------------------------------------------------------------------------
// Civil time
// ---------------------------------------------------------------------------

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant's algorithm).
pub(crate) fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `(year, month, day)` of a day number (inverse of [`days_from_civil`]).
pub(crate) fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// Unix seconds of a UTC civil time.
pub(crate) fn unix_from_civil(y: i64, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
    days_from_civil(y, mo, d) * 86_400 + i64::from(h) * 3600 + i64::from(mi) * 60 + i64::from(s)
}

/// `YYYYMMDDHHMMSS` (UTC) of a Unix timestamp, as used by FTP `MFMT`/`MDTM`.
pub(crate) fn ftp_timestamp(t: i64) -> String {
    let (y, mo, d) = civil_from_days(t.div_euclid(86_400));
    let secs = t.rem_euclid(86_400);
    format!(
        "{y:04}{mo:02}{d:02}{:02}{:02}{:02}",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// Parses `YYYYMMDDHHMMSS[.fff]` (UTC).
pub(crate) fn parse_ftp_timestamp(s: &str) -> Option<i64> {
    let digits = s.split('.').next()?;
    if digits.len() != 14 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = |r: std::ops::Range<usize>| digits[r].parse::<u32>().ok();
    let (y, mo, d, h, mi, sec) = (
        n(0..4)?,
        n(4..6)?,
        n(6..8)?,
        n(8..10)?,
        n(10..12)?,
        n(12..14)?,
    );
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    Some(unix_from_civil(i64::from(y), mo, d, h, mi, sec))
}

fn year_of(t: i64) -> i64 {
    civil_from_days(t.div_euclid(86_400)).0
}

fn month_number(s: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    if s.len() != 3 {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    MONTHS
        .iter()
        .position(|m| *m == lower)
        .map(|i| i as u32 + 1)
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn parse_day(s: &str) -> Option<u32> {
    let d: u32 = s.parse().ok()?;
    (1..=31).contains(&d).then_some(d)
}

/// `HH:MM` (also `HH:MM:SS`).
fn parse_hhmm(s: &str) -> Option<(u32, u32, u32)> {
    let mut it = s.split(':');
    let h: u32 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let sec: u32 = match it.next() {
        Some(x) => x.split('.').next()?.parse().ok()?,
        None => 0,
    };
    if it.next().is_some() || h > 23 || m > 59 || sec > 60 {
        return None;
    }
    Some((h, m, sec))
}

/// `YYYY-MM-DD`
fn parse_iso_date(s: &str) -> Option<(i64, u32, u32)> {
    let mut it = s.split('-');
    let (y, m, d) = (it.next()?, it.next()?, it.next()?);
    if it.next().is_some() || y.len() != 4 || !all_digits(y) {
        return None;
    }
    let (m, d): (u32, u32) = (m.parse().ok()?, d.parse().ok()?);
    ((1..=12).contains(&m) && (1..=31).contains(&d)).then(|| (y.parse().unwrap_or(1970), m, d))
}

// ---------------------------------------------------------------------------
// Tokenizing
// ---------------------------------------------------------------------------

/// Whitespace-separated tokens with their byte offsets.
fn tokens(line: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in line.char_indices() {
        if c.is_whitespace() {
            if let Some(s) = start.take() {
                out.push((s, &line[s..i]));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        out.push((s, &line[s..]));
    }
    out
}

fn looks_like_mode(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 10
        && b"-dlcbpsDn".contains(&b[0])
        && b[1..10].iter().all(|c| b"rwxsStTlL-".contains(c))
}

fn entry(kind: u8, size: u64, mtime: Option<i64>, raw_name: &str) -> Option<RemoteEntry> {
    let mut name = raw_name.trim_end_matches(['\r', '\n']);
    if kind == b'l' {
        if let Some(i) = name.find(" -> ") {
            name = &name[..i];
        }
    }
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    Some(RemoteEntry {
        name: name.to_string(),
        is_dir: kind == b'd',
        size,
        mtime,
    })
}

// ---------------------------------------------------------------------------
// find -printf
// ---------------------------------------------------------------------------

/// Parses records of `find … -printf '%Y %s %T@ %f\0'` (`%y`/`%P` work too). Records are
/// NUL-terminated, or newline-terminated when the output contains no NUL. Type `d` is a
/// directory; every other type (file, symlink, …) is reported as a file.
pub fn parse_find_output(output: &[u8]) -> Vec<RemoteEntry> {
    let sep = if output.contains(&0) { 0u8 } else { b'\n' };
    let mut out = Vec::new();
    for record in output.split(|b| *b == sep) {
        let record = String::from_utf8_lossy(record);
        let record = if sep == b'\n' {
            record.trim_end_matches('\r')
        } else {
            &record
        };
        let mut parts = record.splitn(4, ' ');
        let (Some(kind), Some(size), Some(mtime), Some(name)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let kind = kind.trim_start_matches('\n');
        if kind.len() != 1 {
            continue;
        }
        let size = size.parse().unwrap_or(0);
        let mtime = mtime.split('.').next().and_then(|s| s.parse::<i64>().ok());
        if let Some(e) = entry(if kind == "d" { b'd' } else { b'f' }, size, mtime, name) {
            out.push(e);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// ls -l / FTP LIST
// ---------------------------------------------------------------------------

/// Parses `ls -l`-style and MS-DOS-style listing lines (FTP `LIST`, `ls -la`). Lines that are
/// not entries (`total 12`, blank lines, `.`/`..`) are skipped. `now` (Unix seconds) resolves
/// dates printed without a year.
pub fn parse_ls_output(text: &str, now: i64) -> Vec<RemoteEntry> {
    text.lines()
        .filter_map(|line| parse_list_line(line, now))
        .collect()
}

/// One `ls -l` / FTP `LIST` line (Unix or MS-DOS style).
pub fn parse_list_line(line: &str, now: i64) -> Option<RemoteEntry> {
    let line = line.trim_end_matches(['\r', '\n']);
    parse_unix_line(line, now).or_else(|| parse_dos_line(line))
}

fn parse_unix_line(line: &str, now: i64) -> Option<RemoteEntry> {
    let toks = tokens(line);
    let mode = toks.first()?.1;
    if !looks_like_mode(mode) {
        return None;
    }
    let kind = mode.as_bytes()[0];
    // `--time-style=+%s`: mode links owner group size epoch name
    if toks.len() >= 7 && all_digits(toks[4].1) && all_digits(toks[5].1) && toks[5].1.len() >= 9 {
        return entry(
            kind,
            toks[4].1.parse().ok()?,
            toks[5].1.parse().ok(),
            &line[toks[6].0..],
        );
    }
    // `Mon DD HH:MM name` / `Mon DD YYYY name`, preceded by the size. The usual column (5)
    // and the no-group column (4) are tried first, so owners named like months are harmless.
    let last = toks.len().saturating_sub(3);
    let candidates = [5, 4]
        .into_iter()
        .filter(|&i| i < last)
        .chain((2..last).filter(|&i| i != 4 && i != 5));
    for i in candidates {
        let (Some(month), Some(day)) = (month_number(toks[i].1), parse_day(toks[i + 1].1)) else {
            continue;
        };
        if !all_digits(toks[i - 1].1) {
            continue;
        }
        let when = toks[i + 2].1;
        let mtime = if let Some((h, mi, s)) = parse_hhmm(when) {
            let mut year = year_of(now);
            let mut t = unix_from_civil(year, month, day, h, mi, s);
            if t > now + 2 * 86_400 {
                year -= 1;
                t = unix_from_civil(year, month, day, h, mi, s);
            }
            t
        } else if when.len() == 4 && all_digits(when) {
            unix_from_civil(when.parse().ok()?, month, day, 0, 0, 0)
        } else {
            continue;
        };
        let size = toks[i - 1].1.parse().ok()?;
        return entry(kind, size, Some(mtime), &line[toks[i + 3].0..]);
    }
    // `YYYY-MM-DD HH:MM name` (long-iso), preceded by the size.
    for i in 2..toks.len().saturating_sub(2) {
        let (Some((y, mo, d)), Some((h, mi, s))) =
            (parse_iso_date(toks[i].1), parse_hhmm(toks[i + 1].1))
        else {
            continue;
        };
        if !all_digits(toks[i - 1].1) {
            continue;
        }
        let size = toks[i - 1].1.parse().ok()?;
        return entry(
            kind,
            size,
            Some(unix_from_civil(y, mo, d, h, mi, s)),
            &line[toks[i + 2].0..],
        );
    }
    None
}

/// `01-15-24  10:30AM  <DIR>  name` / `01-15-2024  10:30PM  1,234 name` / `2024-01-15 10:30 <DIR> name`.
fn parse_dos_line(line: &str) -> Option<RemoteEntry> {
    let toks = tokens(line);
    if toks.len() < 4 {
        return None;
    }
    let date = toks[0].1;
    let fields: Vec<&str> = date.split(['-', '/']).collect();
    if fields.len() != 3 || !fields.iter().all(|f| all_digits(f)) {
        return None;
    }
    let (y, mo, d) = if fields[0].len() == 4 {
        (
            fields[0].parse::<i64>().ok()?,
            fields[1].parse::<u32>().ok()?,
            fields[2].parse::<u32>().ok()?,
        )
    } else {
        let y: i64 = fields[2].parse().ok()?;
        let y = match fields[2].len() {
            2 if y < 70 => 2000 + y,
            2 => 1900 + y,
            _ => y,
        };
        (y, fields[0].parse().ok()?, fields[1].parse().ok()?)
    };
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let mut idx = 1;
    let time = toks[idx].1.to_ascii_uppercase();
    let (time, mut pm, mut am) = (
        time.trim_end_matches("AM")
            .trim_end_matches("PM")
            .to_string(),
        time.ends_with("PM"),
        time.ends_with("AM"),
    );
    idx += 1;
    if let Some(&(_, ampm)) = toks.get(idx) {
        if ampm.eq_ignore_ascii_case("AM") || ampm.eq_ignore_ascii_case("PM") {
            pm = ampm.eq_ignore_ascii_case("PM");
            am = !pm;
            idx += 1;
        }
    }
    let (mut h, mi, _) = parse_hhmm(&time)?;
    if pm && h < 12 {
        h += 12;
    } else if am && h == 12 {
        h = 0;
    }
    let what = toks.get(idx)?.1;
    let name_tok = toks.get(idx + 1)?;
    let (kind, size) = if what.eq_ignore_ascii_case("<DIR>") {
        (b'd', 0)
    } else {
        let digits = what.replace(',', "");
        if !all_digits(&digits) {
            return None;
        }
        (b'-', digits.parse().ok()?)
    };
    entry(
        kind,
        size,
        Some(unix_from_civil(y, mo, d, h, mi, 0)),
        &line[name_tok.0..],
    )
}

// ---------------------------------------------------------------------------
// MLSD
// ---------------------------------------------------------------------------

/// Parses FTP `MLSD` lines: `type=file;size=12;modify=20240115103000; name`.
pub fn parse_mlsd_output(text: &str) -> Vec<RemoteEntry> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        let Some((facts, name)) = line.split_once(' ') else {
            continue;
        };
        let mut kind = String::new();
        let mut size = 0u64;
        let mut mtime = None;
        for fact in facts.split(';') {
            let Some((key, value)) = fact.split_once('=') else {
                continue;
            };
            match key.to_ascii_lowercase().as_str() {
                "type" => kind = value.to_ascii_lowercase(),
                "size" | "sizd" => size = value.parse().unwrap_or(0),
                "modify" => mtime = parse_ftp_timestamp(value),
                _ => {}
            }
        }
        if kind == "cdir" || kind == "pdir" {
            continue;
        }
        if let Some(e) = entry(if kind == "dir" { b'd' } else { b'-' }, size, mtime, name) {
            out.push(e);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2024-03-10T12:00:00Z
    const NOW: i64 = 1_710_072_000;

    fn e(name: &str, is_dir: bool, size: u64, mtime: i64) -> RemoteEntry {
        RemoteEntry {
            name: name.into(),
            is_dir,
            size,
            mtime: Some(mtime),
        }
    }

    #[test]
    fn civil_conversions() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(unix_from_civil(2024, 1, 15, 10, 30, 0), 1_705_314_600);
        assert_eq!(unix_from_civil(2024, 3, 10, 12, 0, 0), NOW);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        assert_eq!(
            civil_from_days(days_from_civil(1969, 12, 31)),
            (1969, 12, 31)
        );
        for z in [-800_000, -1, 0, 59, 11_016, 19_737, 2_932_896] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(ftp_timestamp(1_705_314_600), "20240115103000");
        assert_eq!(parse_ftp_timestamp("20240115103000"), Some(1_705_314_600));
        assert_eq!(
            parse_ftp_timestamp("20240115103000.123"),
            Some(1_705_314_600)
        );
        assert_eq!(parse_ftp_timestamp("2024011510300"), None);
        assert_eq!(parse_ftp_timestamp("20241315103000"), None);
        assert_eq!(year_of(NOW), 2024);
    }

    #[test]
    fn find_records() {
        let out = b"d 4096 1705314600.1234567890 pkg\0f 12 1705314601.0000000000 a b.py\0l 7 1705314602.5 link\0f 3 1705314603 it's \"x\".txt\0";
        assert_eq!(
            parse_find_output(out),
            vec![
                e("pkg", true, 4096, 1_705_314_600),
                e("a b.py", false, 12, 1_705_314_601),
                e("link", false, 7, 1_705_314_602),
                e("it's \"x\".txt", false, 3, 1_705_314_603)
            ]
        );
        // Newline-terminated variant (the `%f\n` format) and relative paths (`%P`).
        let out = b"f 1 1705314600.0 sub/dir/x y.py\nd 0 1705314600.0 sub\n";
        assert_eq!(
            parse_find_output(out),
            vec![
                e("sub/dir/x y.py", false, 1, 1_705_314_600),
                e("sub", true, 0, 1_705_314_600)
            ]
        );
        assert!(parse_find_output(b"").is_empty());
        assert!(parse_find_output(b"garbage\0d 1 2 .\0").is_empty());
    }

    #[test]
    fn ls_la_output() {
        let text = "total 24\n\
drwxr-xr-x  4 deploy www-data 4096 Mar  9 14:05 .\n\
drwxr-xr-x  3 root   root     4096 Jan  2  2023 ..\n\
-rw-r--r--  1 deploy www-data  123 Jan 15 10:30 app.py\n\
-rw-r--r--  1 deploy www-data    7 Dec 24 18:00 old  name.txt\n\
drwxr-xr-x+ 2 deploy www-data 4096 Jun  1  2022 static\n\
lrwxrwxrwx  1 deploy www-data   11 Mar 10 09:15 current -> releases/42\n\
-rw-r--r--@ 1 me     staff      10 Feb 29 12:00 leap.txt\n";
        let got = parse_ls_output(text, NOW);
        assert_eq!(
            got,
            vec![
                e(
                    "app.py",
                    false,
                    123,
                    unix_from_civil(2024, 1, 15, 10, 30, 0)
                ),
                e(
                    "old  name.txt",
                    false,
                    7,
                    unix_from_civil(2023, 12, 24, 18, 0, 0)
                ),
                e("static", true, 4096, unix_from_civil(2022, 6, 1, 0, 0, 0)),
                e("current", false, 11, unix_from_civil(2024, 3, 10, 9, 15, 0)),
                e(
                    "leap.txt",
                    false,
                    10,
                    unix_from_civil(2024, 2, 29, 12, 0, 0)
                ),
            ]
        );
    }

    #[test]
    fn ls_time_styles() {
        let epoch = "-rw-r--r-- 1 deploy deploy 42 1705314600 my file.py\ndrwxr-xr-x 2 1000 1000 4096 1705314000 dir name";
        assert_eq!(
            parse_ls_output(epoch, NOW),
            vec![
                e("my file.py", false, 42, 1_705_314_600),
                e("dir name", true, 4096, 1_705_314_000)
            ]
        );
        let iso = "-rw-r--r-- 1 deploy deploy 42 2024-01-15 10:30 x.py";
        assert_eq!(
            parse_ls_output(iso, NOW),
            vec![e("x.py", false, 42, 1_705_314_600)]
        );
        // Missing group column, device numbers.
        let odd = "-rw-r--r--   1 owner    1234 Jan 15 10:30 nogroup.txt\ncrw-rw-rw- 1 root root 1, 3 Jan 15 10:30 null\n-rw-r--r-- 1 may 15 1234 Jan 15 10:30 Mar 3 12:00 notes";
        assert_eq!(
            parse_ls_output(odd, NOW),
            vec![
                e("nogroup.txt", false, 1234, 1_705_314_600),
                e("null", false, 3, 1_705_314_600),
                e("Mar 3 12:00 notes", false, 1234, 1_705_314_600)
            ]
        );
    }

    #[test]
    fn ftp_list_unix_and_dos() {
        let text = "drwxr-xr-x    2 ftp      ftp          4096 Jan 15 10:30 public_html\r\n\
-rw-r--r--    1 ftp      ftp           512 Jan 15  2021 index.html\r\n";
        assert_eq!(
            parse_ls_output(text, NOW),
            vec![
                e("public_html", true, 4096, 1_705_314_600),
                e(
                    "index.html",
                    false,
                    512,
                    unix_from_civil(2021, 1, 15, 0, 0, 0)
                )
            ]
        );
        let dos = "01-15-24  10:30AM       <DIR>          My Docs\r\n01-15-2024  10:30PM              1,234 report final.txt\r\n02-01-99  12:05AM  5 old.txt\r\n2024-01-15  10:30  <DIR>  iso dir\r\n01-15-24  12:30 PM  10 spaced.txt";
        assert_eq!(
            parse_ls_output(dos, NOW),
            vec![
                e("My Docs", true, 0, 1_705_314_600),
                e(
                    "report final.txt",
                    false,
                    1234,
                    unix_from_civil(2024, 1, 15, 22, 30, 0)
                ),
                e("old.txt", false, 5, unix_from_civil(1999, 2, 1, 0, 5, 0)),
                e("iso dir", true, 0, 1_705_314_600),
                e(
                    "spaced.txt",
                    false,
                    10,
                    unix_from_civil(2024, 1, 15, 12, 30, 0)
                ),
            ]
        );
        assert_eq!(parse_list_line("total 0", NOW), None);
        assert_eq!(parse_list_line("", NOW), None);
        assert_eq!(
            parse_list_line("ls: cannot access 'x': No such file or directory", NOW),
            None
        );
    }

    #[test]
    fn mlsd() {
        let text = "type=cdir;modify=20240115103000;perm=el; .\r\n\
type=pdir;modify=20240115103000;perm=el; ..\r\n\
type=dir;modify=20240115103000;perm=el; static files\r\n\
type=file;size=123;modify=20240115103001.250;UNIX.mode=0644; it's a file.py\r\n\
Type=OS.unix=slink:/etc;Modify=20240115103002; etc-link\r\n";
        assert_eq!(
            parse_mlsd_output(text),
            vec![
                e("static files", true, 0, 1_705_314_600),
                e("it's a file.py", false, 123, 1_705_314_601),
                e("etc-link", false, 0, 1_705_314_602)
            ]
        );
    }
}
