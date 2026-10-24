//! Just enough X.509 (DER) parsing to show a certificate to the user: subject,
//! issuer, validity, serial number, alternative names, key and signature
//! algorithms, and SHA-256 / SHA-1 fingerprints. Verification is rustls' job;
//! this parser never panics and rejects anything it does not understand.

use std::fmt;

/// A distinguished name as `(short attribute name, value)` pairs in certificate order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DistinguishedName {
    pub attributes: Vec<(String, String)>,
}

impl DistinguishedName {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn common_name(&self) -> Option<&str> {
        self.get("CN")
    }

    pub fn organization(&self) -> Option<&str> {
        self.get("O")
    }

    pub fn is_empty(&self) -> bool {
        self.attributes.is_empty()
    }
}

impl fmt::Display for DistinguishedName {
    /// Most specific attribute first (`CN=…, O=…, C=…`), as RFC 4514 prints names.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for (k, v) in self.attributes.iter().rev() {
            if !first {
                f.write_str(", ")?;
            }
            first = false;
            write!(f, "{k}={v}")?;
        }
        Ok(())
    }
}

/// What the UI shows about a certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CertificateInfo {
    pub subject: DistinguishedName,
    pub issuer: DistinguishedName,
    /// Serial number as colon-separated hex bytes.
    pub serial: String,
    /// Validity period as Unix seconds.
    pub not_before: Option<i64>,
    pub not_after: Option<i64>,
    /// Subject Alternative Name DNS entries.
    pub dns_names: Vec<String>,
    /// Subject Alternative Name IP addresses.
    pub ip_addresses: Vec<String>,
    /// Basic Constraints `cA` flag: a certificate authority.
    pub is_ca: bool,
    /// Issuer and subject are the same name.
    pub self_signed: bool,
    pub signature_algorithm: String,
    /// E.g. "RSA 2048 bits", "EC P-256", "Ed25519".
    pub public_key: String,
    pub sha256: [u8; 32],
    pub sha1: [u8; 20],
    /// Set when the certificate could not be parsed (only the fingerprints are known).
    pub parse_error: Option<String>,
}

impl CertificateInfo {
    /// Parses a DER certificate. Malformed input still yields the fingerprints,
    /// with [`CertificateInfo::parse_error`] set.
    pub fn from_der(der: &[u8]) -> CertificateInfo {
        let sha256 = sha256(der);
        let sha1 = sha1(der);
        match parse_certificate(der) {
            Ok(mut info) => {
                info.sha256 = sha256;
                info.sha1 = sha1;
                info
            }
            Err(e) => CertificateInfo {
                subject: DistinguishedName::default(),
                issuer: DistinguishedName::default(),
                serial: String::new(),
                not_before: None,
                not_after: None,
                dns_names: Vec::new(),
                ip_addresses: Vec::new(),
                is_ca: false,
                self_signed: false,
                signature_algorithm: String::new(),
                public_key: String::new(),
                sha256,
                sha1,
                parse_error: Some(e),
            },
        }
    }

    /// Short name for lists: the subject CN, else the first DNS name, else the whole subject.
    pub fn display_name(&self) -> String {
        if let Some(cn) = self.subject.common_name().filter(|c| !c.trim().is_empty()) {
            return cn.to_string();
        }
        if let Some(d) = self.dns_names.first() {
            return d.clone();
        }
        if !self.subject.is_empty() {
            return self.subject.to_string();
        }
        format!("Certificate {}", &hex_colon(&self.sha256)[..23])
    }

    pub fn issuer_name(&self) -> String {
        self.issuer
            .common_name()
            .or_else(|| self.issuer.organization())
            .map(str::to_string)
            .unwrap_or_else(|| self.issuer.to_string())
    }

    pub fn sha256_hex(&self) -> String {
        hex_colon(&self.sha256)
    }

    pub fn sha1_hex(&self) -> String {
        hex_colon(&self.sha1)
    }

    pub fn is_expired(&self, now_unix: i64) -> bool {
        self.not_after.is_some_and(|t| t < now_unix)
    }

    pub fn is_not_yet_valid(&self, now_unix: i64) -> bool {
        self.not_before.is_some_and(|t| t > now_unix)
    }

    /// "Valid from 2024-01-01 to 2025-01-01" style period (dates only).
    pub fn validity_text(&self) -> String {
        let d = |t: Option<i64>| {
            t.map(|t| format_time(t)[..10].to_string())
                .unwrap_or_else(|| "?".into())
        };
        format!("{} – {}", d(self.not_before), d(self.not_after))
    }

    /// Subject Alternative Names as one line (`DNS: a, b; IP: 1.2.3.4`).
    pub fn alt_names_text(&self) -> String {
        let mut parts = Vec::new();
        if !self.dns_names.is_empty() {
            parts.push(format!("DNS: {}", self.dns_names.join(", ")));
        }
        if !self.ip_addresses.is_empty() {
            parts.push(format!("IP: {}", self.ip_addresses.join(", ")));
        }
        parts.join("; ")
    }
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let d = ring::digest::digest(&ring::digest::SHA256, data);
    let mut out = [0u8; 32];
    out.copy_from_slice(d.as_ref());
    out
}

pub fn sha1(data: &[u8]) -> [u8; 20] {
    let d = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, data);
    let mut out = [0u8; 20];
    out.copy_from_slice(d.as_ref());
    out
}

/// `AB:CD:…` upper-case hex.
pub fn hex_colon(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// `2026-10-10 12:00:00 UTC`.
pub fn format_time(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------------------------------------------------------------------------
// DER reader

const SEQUENCE: u8 = 0x30;
const SET: u8 = 0x31;
const INTEGER: u8 = 0x02;
const BIT_STRING: u8 = 0x03;
const OCTET_STRING: u8 = 0x04;
const OID: u8 = 0x06;
const BOOLEAN: u8 = 0x01;
const UTC_TIME: u8 = 0x17;
const GENERALIZED_TIME: u8 = 0x18;

struct Der<'a> {
    data: &'a [u8],
}

impl<'a> Der<'a> {
    fn new(data: &'a [u8]) -> Self {
        Der { data }
    }

    fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    fn peek_tag(&self) -> Option<u8> {
        self.data.first().copied()
    }

    /// Reads one TLV: `(tag, contents)`.
    fn read(&mut self) -> Result<(u8, &'a [u8]), String> {
        let d = self.data;
        if d.len() < 2 {
            return Err("truncated DER".into());
        }
        let tag = d[0];
        if tag & 0x1f == 0x1f {
            return Err("unsupported DER tag".into());
        }
        let (len, header) = match d[1] {
            n if n < 0x80 => (n as usize, 2),
            0x80 => return Err("indefinite DER length".into()),
            n => {
                let count = (n & 0x7f) as usize;
                if count > 4 || d.len() < 2 + count {
                    return Err("bad DER length".into());
                }
                let mut len = 0usize;
                for b in &d[2..2 + count] {
                    len = (len << 8) | *b as usize;
                }
                (len, 2 + count)
            }
        };
        let end = header
            .checked_add(len)
            .filter(|e| *e <= d.len())
            .ok_or("truncated DER")?;
        self.data = &d[end..];
        Ok((tag, &d[header..end]))
    }

    fn expect(&mut self, tag: u8, what: &str) -> Result<&'a [u8], String> {
        let (t, v) = self.read()?;
        if t != tag {
            return Err(format!("expected {what}"));
        }
        Ok(v)
    }

    /// Reads a TLV and also returns its full encoding (header included).
    fn read_raw(&mut self) -> Result<(u8, &'a [u8], &'a [u8]), String> {
        let before = self.data;
        let (t, v) = self.read()?;
        let used = before.len() - self.data.len();
        Ok((t, v, &before[..used]))
    }
}

fn oid_string(bytes: &[u8]) -> String {
    let mut parts: Vec<u64> = Vec::new();
    let mut acc: u64 = 0;
    for (i, b) in bytes.iter().enumerate() {
        acc = (acc << 7) | (b & 0x7f) as u64;
        if b & 0x80 == 0 {
            if parts.is_empty() {
                let (a, rest) = if acc < 40 {
                    (0, acc)
                } else if acc < 80 {
                    (1, acc - 40)
                } else {
                    (2, acc - 80)
                };
                parts.push(a);
                parts.push(rest);
            } else {
                parts.push(acc);
            }
            acc = 0;
        } else if i == bytes.len() - 1 || acc > u64::MAX >> 7 {
            break;
        }
    }
    parts
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(".")
}

fn attribute_name(oid: &str) -> String {
    match oid {
        "2.5.4.3" => "CN",
        "2.5.4.4" => "SN",
        "2.5.4.5" => "SERIALNUMBER",
        "2.5.4.6" => "C",
        "2.5.4.7" => "L",
        "2.5.4.8" => "ST",
        "2.5.4.9" => "STREET",
        "2.5.4.10" => "O",
        "2.5.4.11" => "OU",
        "2.5.4.12" => "T",
        "2.5.4.42" => "GN",
        "1.2.840.113549.1.9.1" => "E",
        "0.9.2342.19200300.100.1.25" => "DC",
        "0.9.2342.19200300.100.1.1" => "UID",
        other => return other.to_string(),
    }
    .to_string()
}

fn algorithm_name(oid: &str) -> String {
    match oid {
        "1.2.840.113549.1.1.1" => "RSA",
        "1.2.840.113549.1.1.5" => "SHA1withRSA",
        "1.2.840.113549.1.1.10" => "RSASSA-PSS",
        "1.2.840.113549.1.1.11" => "SHA256withRSA",
        "1.2.840.113549.1.1.12" => "SHA384withRSA",
        "1.2.840.113549.1.1.13" => "SHA512withRSA",
        "1.2.840.10045.2.1" => "EC",
        "1.2.840.10045.4.1" => "SHA1withECDSA",
        "1.2.840.10045.4.3.2" => "SHA256withECDSA",
        "1.2.840.10045.4.3.3" => "SHA384withECDSA",
        "1.2.840.10045.4.3.4" => "SHA512withECDSA",
        "1.3.101.112" => "Ed25519",
        "1.3.101.113" => "Ed448",
        other => return other.to_string(),
    }
    .to_string()
}

fn curve_name(oid: &str) -> String {
    match oid {
        "1.2.840.10045.3.1.7" => "P-256",
        "1.3.132.0.34" => "P-384",
        "1.3.132.0.35" => "P-521",
        "1.3.132.0.10" => "secp256k1",
        other => return other.to_string(),
    }
    .to_string()
}

/// A DirectoryString (or similar) value as text.
fn string_value(tag: u8, v: &[u8]) -> String {
    match tag {
        // UTF8String, PrintableString, IA5String, VisibleString, NumericString
        0x0c | 0x13 | 0x16 | 0x1a | 0x12 => String::from_utf8_lossy(v).into_owned(),
        // TeletexString: treat as Latin-1.
        0x14 => v.iter().map(|b| *b as char).collect(),
        // BMPString: UTF-16BE.
        0x1e => {
            let units: Vec<u16> = v
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        // UniversalString: UTF-32BE.
        0x1c => v
            .as_chunks::<4>()
            .0
            .iter()
            .filter_map(|c| char::from_u32(u32::from_be_bytes([c[0], c[1], c[2], c[3]])))
            .collect(),
        _ => hex_colon(v),
    }
}

fn parse_name(v: &[u8]) -> Result<DistinguishedName, String> {
    let mut rdns = Der::new(v);
    let mut attributes = Vec::new();
    while !rdns.is_empty() {
        let set = rdns.expect(SET, "RDN set")?;
        let mut atvs = Der::new(set);
        while !atvs.is_empty() {
            let atv = atvs.expect(SEQUENCE, "attribute")?;
            let mut a = Der::new(atv);
            let oid = oid_string(a.expect(OID, "attribute type")?);
            let (tag, value) = a.read()?;
            attributes.push((attribute_name(&oid), string_value(tag, value)));
        }
    }
    Ok(DistinguishedName { attributes })
}

fn digits(s: &[u8]) -> Option<i64> {
    if s.is_empty() || !s.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(s).ok()?.parse().ok()
}

/// UTCTime (`YYMMDDHHMM[SS]Z`) or GeneralizedTime (`YYYYMMDDHHMMSS[.f]Z`) as Unix seconds.
fn parse_time(tag: u8, v: &[u8]) -> Option<i64> {
    let (year, rest) = match tag {
        UTC_TIME => {
            let yy = digits(v.get(..2)?)?;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, v.get(2..)?)
        }
        GENERALIZED_TIME => (digits(v.get(..4)?)?, v.get(4..)?),
        _ => return None,
    };
    let month = digits(rest.get(0..2)?)?;
    let day = digits(rest.get(2..4)?)?;
    let hour = digits(rest.get(4..6)?)?;
    let minute = digits(rest.get(6..8)?)?;
    let second = rest.get(8..10).and_then(digits).unwrap_or(0);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

fn describe_public_key(spki: &[u8]) -> Result<String, String> {
    let mut s = Der::new(spki);
    let alg = s.expect(SEQUENCE, "algorithm")?;
    let key = s.expect(BIT_STRING, "public key")?;
    let mut a = Der::new(alg);
    let oid = oid_string(a.expect(OID, "algorithm id")?);
    Ok(match oid.as_str() {
        "1.2.840.113549.1.1.1" => {
            // BIT STRING: unused-bits byte, then RSAPublicKey ::= SEQUENCE { n INTEGER, e INTEGER }.
            let bits = key.get(1..).and_then(|k| {
                let mut r = Der::new(k);
                let seq = r.expect(SEQUENCE, "RSA key").ok()?;
                let n = Der::new(seq).expect(INTEGER, "modulus").ok()?;
                let n = match n.iter().position(|b| *b != 0) {
                    Some(i) => &n[i..],
                    None => return Some(0),
                };
                Some((n.len() - 1) * 8 + (8 - n[0].leading_zeros() as usize))
            });
            match bits {
                Some(b) => format!("RSA {b} bits"),
                None => "RSA".into(),
            }
        }
        "1.2.840.10045.2.1" => match a.read() {
            Ok((OID, curve)) => format!("EC {}", curve_name(&oid_string(curve))),
            _ => "EC".into(),
        },
        other => algorithm_name(other),
    })
}

fn parse_extensions(v: &[u8], info: &mut CertificateInfo) -> Result<(), String> {
    let mut outer = Der::new(v);
    let list = outer.expect(SEQUENCE, "extensions")?;
    let mut exts = Der::new(list);
    while !exts.is_empty() {
        let ext = exts.expect(SEQUENCE, "extension")?;
        let mut e = Der::new(ext);
        let oid = oid_string(e.expect(OID, "extension id")?);
        if e.peek_tag() == Some(BOOLEAN) {
            e.read()?;
        }
        let value = e.expect(OCTET_STRING, "extension value")?;
        match oid.as_str() {
            // Subject Alternative Name: SEQUENCE OF GeneralName.
            "2.5.29.17" => {
                let mut v = Der::new(value);
                let names = v.expect(SEQUENCE, "alt names")?;
                let mut names = Der::new(names);
                while !names.is_empty() {
                    let (tag, n) = names.read()?;
                    match tag {
                        0x82 => info.dns_names.push(String::from_utf8_lossy(n).into_owned()),
                        0x87 => match n.len() {
                            4 => info
                                .ip_addresses
                                .push(std::net::Ipv4Addr::new(n[0], n[1], n[2], n[3]).to_string()),
                            16 => {
                                let mut b = [0u8; 16];
                                b.copy_from_slice(n);
                                info.ip_addresses
                                    .push(std::net::Ipv6Addr::from(b).to_string());
                            }
                            _ => {}
                        },
                        _ => {}
                    }
                }
            }
            // Basic Constraints: SEQUENCE { cA BOOLEAN DEFAULT FALSE, ... }.
            "2.5.29.19" => {
                let mut v = Der::new(value);
                let bc = v.expect(SEQUENCE, "basic constraints")?;
                let mut bc = Der::new(bc);
                if bc.peek_tag() == Some(BOOLEAN) {
                    let (_, flag) = bc.read()?;
                    info.is_ca = flag.first().is_some_and(|b| *b != 0);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn parse_certificate(der: &[u8]) -> Result<CertificateInfo, String> {
    let mut top = Der::new(der);
    let cert = top.expect(SEQUENCE, "certificate")?;
    let mut c = Der::new(cert);
    let tbs = c.expect(SEQUENCE, "TBSCertificate")?;
    let sig_alg = c.expect(SEQUENCE, "signature algorithm")?;
    let signature_algorithm = Der::new(sig_alg)
        .expect(OID, "signature algorithm id")
        .map(|o| algorithm_name(&oid_string(o)))?;

    let mut t = Der::new(tbs);
    if t.peek_tag() == Some(0xa0) {
        t.read()?; // [0] EXPLICIT version
    }
    let serial = t.expect(INTEGER, "serial number")?;
    let serial = match serial.iter().position(|b| *b != 0) {
        Some(i) => hex_colon(&serial[i..]),
        None => "00".into(),
    };
    t.expect(SEQUENCE, "signature")?;
    let (issuer_tag, issuer, issuer_raw) = t.read_raw()?;
    if issuer_tag != SEQUENCE {
        return Err("expected issuer".into());
    }
    let validity = t.expect(SEQUENCE, "validity")?;
    let (subject_tag, subject, subject_raw) = t.read_raw()?;
    if subject_tag != SEQUENCE {
        return Err("expected subject".into());
    }
    let spki = t.expect(SEQUENCE, "public key info")?;
    let mut v = Der::new(validity);
    let (nb_tag, nb) = v.read()?;
    let (na_tag, na) = v.read()?;
    let mut info = CertificateInfo {
        subject: parse_name(subject)?,
        issuer: parse_name(issuer)?,
        serial,
        not_before: parse_time(nb_tag, nb),
        not_after: parse_time(na_tag, na),
        dns_names: Vec::new(),
        ip_addresses: Vec::new(),
        is_ca: false,
        self_signed: issuer_raw == subject_raw,
        signature_algorithm,
        public_key: describe_public_key(spki)?,
        sha256: [0; 32],
        sha1: [0; 20],
        parse_error: None,
    };
    while !t.is_empty() {
        let (tag, value) = t.read()?;
        if tag == 0xa3 {
            parse_extensions(value, &mut info)?;
        }
    }
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_certs::*;

    #[test]
    fn parses_a_self_signed_server_certificate() {
        let der = pem_to_der(SELF_SIGNED_PEM);
        let info = CertificateInfo::from_der(&der);
        assert_eq!(info.parse_error, None);
        assert_eq!(info.subject.common_name(), Some("localhost"));
        assert_eq!(info.subject.organization(), Some("RustPy Test"));
        assert_eq!(info.subject.get("C"), Some("DE"));
        assert!(info.self_signed);
        assert!(!info.is_ca);
        assert_eq!(
            info.dns_names,
            vec!["localhost".to_string(), "self.test".to_string()]
        );
        assert_eq!(info.ip_addresses, vec!["127.0.0.1".to_string()]);
        assert_eq!(info.public_key, "EC P-256");
        assert_eq!(info.signature_algorithm, "SHA256withECDSA");
        assert_eq!(info.display_name(), "localhost");
        assert_eq!(
            info.subject.to_string(),
            "CN=localhost, O=RustPy Test, C=DE"
        );
        // Valid from 2026 for 100 years: the end date needs GeneralizedTime.
        let (nb, na) = (info.not_before.unwrap(), info.not_after.unwrap());
        assert!(format_time(nb).starts_with("2026-"), "{}", format_time(nb));
        assert!(format_time(na).starts_with("2126-"), "{}", format_time(na));
        assert!(
            !info.is_expired(nb + 1) && info.is_expired(na + 1) && info.is_not_yet_valid(nb - 1)
        );
        assert_eq!(info.sha256, sha256(&der));
        assert_eq!(info.sha256_hex().len(), 32 * 3 - 1);
    }

    #[test]
    fn parses_ca_and_leaf_certificates() {
        let ca = CertificateInfo::from_der(&pem_to_der(CA_PEM));
        assert!(ca.is_ca && ca.self_signed, "{ca:?}");
        assert_eq!(ca.subject.common_name(), Some("RustPy Test CA"));
        let leaf = CertificateInfo::from_der(&pem_to_der(LEAF_PEM));
        assert!(!leaf.is_ca && !leaf.self_signed);
        assert_eq!(leaf.issuer.common_name(), Some("RustPy Test CA"));
        assert_eq!(leaf.issuer_name(), "RustPy Test CA");
        assert_eq!(leaf.dns_names, vec!["leaf.test".to_string()]);
        assert_eq!(leaf.alt_names_text(), "DNS: leaf.test");
    }

    #[test]
    fn rsa_key_size() {
        let info = CertificateInfo::from_der(&pem_to_der(RSA_PEM));
        assert_eq!(info.public_key, "RSA 2048 bits");
        assert_eq!(info.signature_algorithm, "SHA256withRSA");
        assert_eq!(info.subject.common_name(), Some("rsa.test"));
        // 2049 ends in UTCTime's range (YY < 50 means 20YY).
        assert!(format_time(info.not_after.unwrap()).starts_with("2049-"));
    }

    #[test]
    fn garbage_keeps_fingerprints() {
        for bad in [
            &b""[..],
            b"\x30",
            b"\x30\x84\xff\xff\xff\xff",
            b"\x30\x03\x02\x01",
            b"not a certificate",
        ] {
            let info = CertificateInfo::from_der(bad);
            assert!(info.parse_error.is_some(), "{bad:?}");
            assert_eq!(info.sha256, sha256(bad));
            assert!(info.display_name().starts_with("Certificate "));
        }
        // Truncations of a real certificate never panic.
        let der = pem_to_der(SELF_SIGNED_PEM);
        for n in 0..der.len() {
            let _ = CertificateInfo::from_der(&der[..n]);
        }
    }

    #[test]
    fn times_and_dates() {
        assert_eq!(parse_time(UTC_TIME, b"700101000000Z"), Some(0));
        assert_eq!(parse_time(UTC_TIME, b"491231235959Z"), Some(2_524_607_999));
        assert_eq!(parse_time(UTC_TIME, b"500101000000Z"), Some(-631_152_000));
        assert_eq!(
            parse_time(GENERALIZED_TIME, b"20261010120000Z"),
            Some(1_791_633_600)
        );
        assert_eq!(parse_time(GENERALIZED_TIME, b"2026101012Z"), None);
        assert_eq!(parse_time(UTC_TIME, b"7013010000Z"), None);
        assert_eq!(format_time(1_791_633_600), "2026-10-10 12:00:00 UTC");
        assert_eq!(format_time(0), "1970-01-01 00:00:00 UTC");
        for days in [-800_000i64, -1, 0, 59, 60, 365, 11_016, 2_932_896] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
    }

    #[test]
    fn oids() {
        assert_eq!(oid_string(&[0x55, 0x04, 0x03]), "2.5.4.3");
        assert_eq!(
            oid_string(&[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b]),
            "1.2.840.113549.1.1.11"
        );
        assert_eq!(
            oid_string(&[0x09, 0x92, 0x26, 0x89, 0x93, 0xf2, 0x2c, 0x64, 0x01, 0x19]),
            "0.9.2342.19200300.100.1.25"
        );
    }
}
