//! Certificate authorities the operating system trusts beyond its CA bundle
//! file, for Tools › Server Certificates › "Trust the operating system's
//! certificate authorities" (see [`crate::certs::system_certificates`]):
//!
//! * **Windows**: the "Trusted Root Certification Authorities" store. The
//!   current user's view of it also lists the machine's roots and those
//!   deployed by group policy, which is where a company's TLS-inspecting
//!   proxy puts its CA.
//! * **macOS**: certificates in the keychains (the System keychain, where
//!   administrators and configuration profiles install CAs, and the user's
//!   login keychain) that the admin or user trust settings mark as trusted
//!   roots, read with the `security` tool. Certificates whose trust settings
//!   deny trust are left out.
//! * Elsewhere the CA bundle files cover it (see `certs`).

/// What "the operating system's certificate authorities" are here, for the settings page.
#[cfg(windows)]
pub const DESCRIPTION: &str = "The Windows certificate store (Trusted Root Certification Authorities of the user and the machine, including those set by group policy)";
#[cfg(target_os = "macos")]
pub const DESCRIPTION: &str = "Unrestricted certificate authorities trusted in the macOS keychains (System and login keychain trust settings) and /etc/ssl/cert.pem; policy-, host- or application-scoped trust requires an explicit import";
#[cfg(not(any(windows, target_os = "macos")))]
pub const DESCRIPTION: &str =
    "The system CA bundle (/etc/ssl/certs/ca-certificates.crt or the distribution's equivalent)";

/// The platform's trusted roots (DER), besides the bundle files.
#[cfg(windows)]
pub(crate) fn platform_certificates() -> Vec<Vec<u8>> {
    use windows_sys::Win32::Foundation::{GetLastError, CRYPT_E_NOT_FOUND};
    use windows_sys::Win32::Security::Cryptography::{
        CertCloseStore, CertEnumCertificatesInStore, CertOpenSystemStoreW,
    };
    let mut out = Vec::new();
    let name: Vec<u16> = "ROOT\0".encode_utf16().collect();
    let denied_name: Vec<u16> = "Disallowed\0".encode_utf16().collect();
    // SAFETY: names are NUL-terminated; every context is read before the next
    // enumeration frees it; both stores are closed after their contexts.
    unsafe {
        let store = CertOpenSystemStoreW(0, name.as_ptr());
        if store.is_null() {
            log::warn!(
                "cannot open the Windows certificate store: {}",
                std::io::Error::last_os_error()
            );
            return out;
        }
        let denied_store = CertOpenSystemStoreW(0, denied_name.as_ptr());
        if denied_store.is_null() {
            log::warn!("cannot inspect Windows distrust settings; no native roots imported");
            CertCloseStore(store, 0);
            return out;
        }
        let mut denied = std::collections::HashSet::new();
        let mut context = CertEnumCertificatesInStore(denied_store, std::ptr::null());
        while !context.is_null() {
            let c = &*context;
            if !c.pbCertEncoded.is_null() && c.cbCertEncoded > 0 {
                denied.insert(crate::x509::sha256(std::slice::from_raw_parts(
                    c.pbCertEncoded,
                    c.cbCertEncoded as usize,
                )));
            }
            context = CertEnumCertificatesInStore(denied_store, context);
        }
        if GetLastError() != CRYPT_E_NOT_FOUND as u32 {
            log::warn!("cannot enumerate Windows distrust settings; no native roots imported");
            CertCloseStore(denied_store, 0);
            CertCloseStore(store, 0);
            return out;
        }
        context = CertEnumCertificatesInStore(store, std::ptr::null());
        while !context.is_null() {
            let c = &*context;
            if !c.pbCertEncoded.is_null()
                && c.cbCertEncoded > 0
                && windows_context_allows_tls(context)
            {
                let der = std::slice::from_raw_parts(c.pbCertEncoded, c.cbCertEncoded as usize);
                if !denied.contains(&crate::x509::sha256(der)) {
                    out.push(der.to_vec());
                }
            }
            context = CertEnumCertificatesInStore(store, context);
        }
        if GetLastError() != CRYPT_E_NOT_FOUND as u32 {
            log::warn!("cannot fully enumerate Windows roots; no native roots imported");
            out.clear();
        }
        CertCloseStore(denied_store, 0);
        CertCloseStore(store, 0);
    }
    out
}

/// The effective EKU is the intersection of the extension and store property
/// (CertGetEnhancedKeyUsage flags=0). Empty + CRYPT_E_NOT_FOUND is unrestricted;
/// empty + success means disabled. Other uses must include TLS server auth.
#[cfg(any(windows, test))]
fn windows_eku_allows_tls(usages: &[&[u8]], unrestricted: bool) -> bool {
    if usages.is_empty() {
        return unrestricted;
    }
    usages
        .iter()
        .any(|oid| *oid == b"1.3.6.1.5.5.7.3.1" || *oid == b"2.5.29.37.0")
}

#[cfg(windows)]
fn windows_context_allows_tls(
    context: *const windows_sys::Win32::Security::Cryptography::CERT_CONTEXT,
) -> bool {
    use windows_sys::Win32::Foundation::{GetLastError, SetLastError, CRYPT_E_NOT_FOUND};
    use windows_sys::Win32::Security::Cryptography::{CertGetEnhancedKeyUsage, CTL_USAGE};
    let mut bytes = 0;
    // SAFETY: context belongs to an open store; a null buffer asks for its size.
    if unsafe { CertGetEnhancedKeyUsage(context, 0, std::ptr::null_mut(), &mut bytes) } == 0
        || bytes < std::mem::size_of::<CTL_USAGE>() as u32
        || bytes > 1024 * 1024
    {
        return false;
    }
    // Word alignment is sufficient for CTL_USAGE and the pointers it holds.
    let mut buffer = vec![0usize; (bytes as usize).div_ceil(std::mem::size_of::<usize>())];
    let usage = buffer.as_mut_ptr().cast::<CTL_USAGE>();
    // SAFETY: buffer has room for the size the API requested, and context stays
    // alive. Read all C strings while the API-owned buffer is still alive.
    unsafe {
        SetLastError(0);
        if CertGetEnhancedKeyUsage(context, 0, usage, &mut bytes) == 0 {
            return false;
        }
        let unrestricted = GetLastError() == CRYPT_E_NOT_FOUND as u32;
        let usage = &*usage;
        let ids = if usage.cUsageIdentifier == 0 {
            &[][..]
        } else {
            if usage.rgpszUsageIdentifier.is_null() {
                return false;
            }
            std::slice::from_raw_parts(usage.rgpszUsageIdentifier, usage.cUsageIdentifier as usize)
        };
        let mut oids = Vec::new();
        for &id in ids {
            if id.is_null() {
                return false;
            }
            oids.push(std::ffi::CStr::from_ptr(id.cast()).to_bytes());
        }
        windows_eku_allows_tls(&oids, unrestricted)
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn platform_certificates() -> Vec<Vec<u8>> {
    keychain_roots(&|args| {
        std::process::Command::new("/usr/bin/security")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| o.stdout)
    })
}

/// macOS: the keychain certificates (the user's search list: the login and
/// the System keychain) that the admin or user trust settings trust as roots.
/// `security` runs the `security` tool with these arguments and returns its
/// output, None when it fails (e.g. a domain without trust settings).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn keychain_roots(security: &dyn Fn(&[&str]) -> Option<Vec<u8>>) -> Vec<Vec<u8>> {
    // The exported trust decisions must not be replaceable by another local
    // user between the security command and the read (or collide in tests).
    let Ok(export_dir) = tempfile::tempdir() else {
        return Vec::new();
    };
    let mut trusted = std::collections::HashSet::new();
    for (i, admin) in [true, false].into_iter().enumerate() {
        let file = export_dir.path().join(format!("trust-{i}.plist"));
        let path = file.to_string_lossy().into_owned();
        let args: &[&str] = if admin {
            &["trust-settings-export", "-d", &path]
        } else {
            &["trust-settings-export", &path]
        };
        if security(args).is_some() {
            if let Ok(xml) = std::fs::read_to_string(&file) {
                // User-domain settings override the admin domain, including
                // explicit deny and scoped trust we cannot safely import.
                for (fingerprint, trust) in trust_settings::root_decisions(&xml) {
                    if trust {
                        trusted.insert(fingerprint);
                    } else {
                        trusted.remove(&fingerprint);
                    }
                }
            }
        }
        let _ = std::fs::remove_file(&file);
    }
    if trusted.is_empty() {
        return Vec::new();
    }
    let Some(pem) = security(&["find-certificate", "-a", "-p"]) else {
        return Vec::new();
    };
    let mut out: Vec<Vec<u8>> = Vec::new();
    for der in crate::certs::pem_certificates(&pem) {
        if trusted.contains(&trust_settings::sha1_hex(&der)) && !out.contains(&der) {
            out.push(der);
        }
    }
    out
}

#[cfg(not(any(windows, target_os = "macos")))]
pub(crate) fn platform_certificates() -> Vec<Vec<u8>> {
    Vec::new()
}

/// macOS trust settings (`security trust-settings-export`): which keychain
/// certificates are trusted roots.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod trust_settings {
    /// Upper-case hex SHA-1 of a certificate, as trust settings name it.
    pub(super) fn sha1_hex(der: &[u8]) -> String {
        crate::x509::sha1(der)
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect()
    }

    /// A value of an XML property list (what trust settings need of it).
    #[derive(Debug, PartialEq)]
    pub(super) enum Plist {
        Dict(Vec<(String, Plist)>),
        Array(Vec<Plist>),
        Integer(i64),
        Other,
    }

    #[derive(Debug, PartialEq)]
    enum Token<'a> {
        Open(&'a str),
        Close(&'a str),
        /// `<name/>`
        Empty(&'a str),
        Text(&'a str),
    }

    fn tokens(xml: &str) -> Vec<Token<'_>> {
        let mut out = Vec::new();
        let mut rest = xml;
        while let Some(start) = rest.find('<') {
            let text = rest[..start].trim();
            if !text.is_empty() {
                out.push(Token::Text(text));
            }
            let Some(end) = rest[start..].find('>') else {
                break;
            };
            let tag = &rest[start + 1..start + end];
            rest = &rest[start + end + 1..];
            if tag.starts_with('?') || tag.starts_with('!') {
                continue;
            }
            if let Some(name) = tag.strip_prefix('/') {
                out.push(Token::Close(name.trim()));
            } else if let Some(name) = tag.strip_suffix('/') {
                out.push(Token::Empty(
                    name.split_whitespace().next().unwrap_or_default(),
                ));
            } else {
                out.push(Token::Open(
                    tag.split_whitespace().next().unwrap_or_default(),
                ));
            }
        }
        out
    }

    fn value(tokens: &[Token], i: &mut usize) -> Option<Plist> {
        let token = tokens.get(*i)?;
        *i += 1;
        match token {
            Token::Empty("dict") => Some(Plist::Dict(Vec::new())),
            Token::Empty("array") => Some(Plist::Array(Vec::new())),
            Token::Empty(_) => Some(Plist::Other),
            Token::Open("dict") => {
                let mut entries = Vec::new();
                loop {
                    match tokens.get(*i)? {
                        Token::Close("dict") => {
                            *i += 1;
                            return Some(Plist::Dict(entries));
                        }
                        Token::Open("key") => {
                            *i += 1;
                            let key = match tokens.get(*i)? {
                                Token::Text(t) => {
                                    *i += 1;
                                    t.to_string()
                                }
                                _ => String::new(),
                            };
                            if tokens.get(*i)? != &Token::Close("key") {
                                return None;
                            }
                            *i += 1;
                            entries.push((key, value(tokens, i)?));
                        }
                        _ => return None,
                    }
                }
            }
            Token::Open("array") => {
                let mut items = Vec::new();
                loop {
                    if tokens.get(*i)? == &Token::Close("array") {
                        *i += 1;
                        return Some(Plist::Array(items));
                    }
                    items.push(value(tokens, i)?);
                }
            }
            Token::Open(name) => {
                // A scalar: its text up to the closing tag.
                let mut text = "";
                while let Some(t) = tokens.get(*i) {
                    *i += 1;
                    match t {
                        Token::Text(s) => text = s,
                        Token::Close(c) if c == name => break,
                        _ => return None,
                    }
                }
                Some(if *name == "integer" {
                    text.parse().map(Plist::Integer).unwrap_or(Plist::Other)
                } else {
                    Plist::Other
                })
            }
            _ => None,
        }
    }

    /// Parses an XML property list (None when it is not one).
    pub(super) fn parse_plist(xml: &str) -> Option<Plist> {
        let tokens = tokens(xml);
        let mut i = tokens.iter().position(|t| *t == Token::Open("plist"))? + 1;
        value(&tokens, &mut i)
    }

    fn get<'a>(dict: &'a [(String, Plist)], key: &str) -> Option<&'a Plist> {
        dict.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Whether a certificate's trust settings make it a trusted root: an empty
    /// list means "always trust"; otherwise some entry must say "trust root" or
    /// "trust as root" (the default result) and none "deny".
    fn trusts_as_root(entry: &Plist) -> bool {
        let Plist::Dict(fields) = entry else {
            return false;
        };
        let Some(Plist::Array(settings)) = get(fields, "trustSettings") else {
            return false;
        };
        if settings.is_empty() {
            return true;
        }
        let results: Vec<i64> = settings
            .iter()
            .map(|s| match s {
                Plist::Dict(f) => match get(f, "kSecTrustSettingsResult") {
                    Some(Plist::Integer(n)) => *n,
                    _ => 1,
                },
                _ => 4,
            })
            .collect();
        // 1 = trust root, 2 = trust as root, 3 = deny, 4 = unspecified.
        !results.contains(&3)
            && settings.iter().zip(results).any(|(setting, result)| {
                let Plist::Dict(fields) = setting else {
                    return false;
                };
                // rustls root stores cannot express OS policy, host, application
                // or key-usage restrictions. Never widen such trust to all TLS.
                (result == 1 || result == 2)
                    && fields.iter().all(|(key, _)| {
                        matches!(
                            key.as_str(),
                            "kSecTrustSettingsResult" | "kSecTrustSettingsAllowedError"
                        )
                    })
            })
    }

    /// SHA-1 fingerprints (upper-case hex) of the certificates that the trust
    /// settings exported by `security trust-settings-export` mark as trusted roots.
    #[cfg(test)]
    pub(super) fn trusted_roots(xml: &str) -> Vec<String> {
        root_decisions(xml)
            .into_iter()
            .filter_map(|(key, trusted)| trusted.then_some(key))
            .collect()
    }

    /// Includes rejected entries so a higher-priority domain can remove trust
    /// granted by a lower-priority domain.
    pub(super) fn root_decisions(xml: &str) -> Vec<(String, bool)> {
        let Some(Plist::Dict(root)) = parse_plist(xml) else {
            return Vec::new();
        };
        let Some(Plist::Dict(list)) = get(&root, "trustList") else {
            return Vec::new();
        };
        list.iter()
            .filter(|(k, _)| k.len() == 40 && k.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(|(k, v)| (k.to_ascii_uppercase(), trusts_as_root(v)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::trust_settings::*;
    use super::{keychain_roots, DESCRIPTION};

    /// What `security trust-settings-export -d` writes (abridged).
    const EXPORT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>trustList</key>
	<dict>
		<key>0A1B2C3D4E5F60718293A4B5C6D7E8F901234567</key>
		<dict>
			<key>issuerName</key>
			<data>MDsxCzAJBgNVBAYTAlVT</data>
			<key>modDate</key>
			<date>2026-01-05T10:00:00Z</date>
			<key>serialNumber</key>
			<data>AQ==</data>
			<key>trustSettings</key>
			<array/>
		</dict>
		<key>1111111111111111111111111111111111111111</key>
		<dict>
			<key>trustSettings</key>
			<array>
				<dict>
					<key>kSecTrustSettingsPolicy</key>
					<data>KoZIhvdjZAED</data>
					<key>kSecTrustSettingsPolicyName</key>
					<string>sslServer</string>
					<key>kSecTrustSettingsResult</key>
					<integer>3</integer>
				</dict>
			</array>
		</dict>
		<key>2222222222222222222222222222222222222222</key>
		<dict>
			<key>trustSettings</key>
			<array>
				<dict>
					<key>kSecTrustSettingsAllowedError</key>
					<integer>-2147409654</integer>
					<key>kSecTrustSettingsResult</key>
					<integer>1</integer>
				</dict>
				<dict>
					<key>kSecTrustSettingsAllowedError</key>
					<integer>-2147408896</integer>
				</dict>
			</array>
		</dict>
		<key>3333333333333333333333333333333333333333</key>
		<dict>
			<key>trustSettings</key>
			<array>
				<dict>
					<key>kSecTrustSettingsResult</key>
					<integer>4</integer>
				</dict>
			</array>
		</dict>
	</dict>
	<key>trustVersion</key>
	<integer>1</integer>
</dict>
</plist>
"#;

    #[test]
    fn windows_usage_restrictions_are_preserved() {
        assert!(
            super::windows_eku_allows_tls(&[], true),
            "no EKU property or extension"
        );
        assert!(
            !super::windows_eku_allows_tls(&[], false),
            "explicitly disabled or disjoint EKUs"
        );
        assert!(
            !super::windows_eku_allows_tls(&[b"1.3.6.1.5.5.7.3.4"], false),
            "email-only root"
        );
        assert!(
            super::windows_eku_allows_tls(&[b"1.3.6.1.5.5.7.3.1"], false),
            "TLS server auth"
        );
        assert!(
            super::windows_eku_allows_tls(&[b"2.5.29.37.0"], false),
            "any key usage"
        );
    }

    #[test]
    fn trust_settings_select_trusted_roots() {
        // Always trusted (empty list) and trusted root; not denied or unspecified ones.
        assert_eq!(
            trusted_roots(EXPORT),
            vec![
                "0A1B2C3D4E5F60718293A4B5C6D7E8F901234567".to_string(),
                "2222222222222222222222222222222222222222".to_string()
            ]
        );
        assert!(trusted_roots("not a plist").is_empty());
        assert!(trusted_roots(
            "<plist><dict><key>trustVersion</key><integer>1</integer></dict></plist>"
        )
        .is_empty());
        // Policy/host/application-scoped trust is not an unrestricted TLS root.
        for key in [
            "kSecTrustSettingsPolicy",
            "kSecTrustSettingsPolicyName",
            "kSecTrustSettingsPolicyString",
            "kSecTrustSettingsApplication",
            "kSecTrustSettingsKeyUsage",
        ] {
            let scoped = EXPORT.replace(
                "<key>kSecTrustSettingsAllowedError</key>",
                &format!("<key>{key}</key>"),
            );
            assert_eq!(
                trusted_roots(&scoped),
                vec!["0A1B2C3D4E5F60718293A4B5C6D7E8F901234567".to_string()],
                "{key}"
            );
        }
        // Truncated output does not panic.
        for end in (0..EXPORT.len()).step_by(97) {
            let _ = trusted_roots(&EXPORT[..end]);
        }
    }

    #[test]
    fn plist_values() {
        let p = parse_plist("<plist><dict><key>a</key><integer>-5</integer><key>b</key><array><true/><string>x</string></array><key></key><dict/></dict></plist>").unwrap();
        assert_eq!(
            p,
            Plist::Dict(vec![
                ("a".into(), Plist::Integer(-5)),
                ("b".into(), Plist::Array(vec![Plist::Other, Plist::Other])),
                (String::new(), Plist::Dict(Vec::new()))
            ])
        );
    }

    /// What `security trust-settings-export` writes for certificates with these
    /// SHA-1 fingerprints and `kSecTrustSettingsResult` values (None: an empty list).
    fn export(entries: &[(&str, Option<i64>)]) -> String {
        let list: String = entries
            .iter()
            .map(|(sha1, result)| {
                let settings = match result {
                    None => "<array/>".to_string(),
                    Some(r) => format!("<array><dict><key>kSecTrustSettingsResult</key><integer>{r}</integer></dict></array>"),
                };
                format!("<key>{sha1}</key><dict><key>trustSettings</key>{settings}</dict>")
            })
            .collect();
        format!("<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>trustList</key><dict>{list}</dict></dict></plist>")
    }

    #[test]
    fn keychain_roots_are_the_trusted_keychain_certificates() {
        use crate::test_certs::{CA_PEM, LEAF_PEM, SELF_SIGNED_PEM};
        let der = |pem: &str| crate::certs::pem_certificates(pem.as_bytes()).remove(0);
        let (ca, leaf, other) = (der(CA_PEM), der(LEAF_PEM), der(SELF_SIGNED_PEM));
        // The admin domain trusts the CA and denies the leaf; the user domain has no settings.
        let admin = export(&[(&sha1_hex(&ca), None), (&sha1_hex(&leaf), Some(3))]);
        let keychains = format!("{CA_PEM}\n{LEAF_PEM}\n{SELF_SIGNED_PEM}\n{CA_PEM}");
        let calls = std::sync::Mutex::new(Vec::new());
        let security = |args: &[&str]| -> Option<Vec<u8>> {
            calls.lock().unwrap().push(args.join(" "));
            match args {
                ["trust-settings-export", "-d", path] => {
                    std::fs::write(path, &admin).unwrap();
                    Some(Vec::new())
                }
                ["find-certificate", "-a", "-p"] => Some(keychains.clone().into_bytes()),
                _ => None,
            }
        };
        let roots = keychain_roots(&security);
        assert_eq!(
            roots,
            vec![ca.clone()],
            "trusted once; not the denied or the unlisted ones"
        );
        assert!(!roots.contains(&other));
        let calls = calls.lock().unwrap().clone();
        assert!(
            calls.len() == 3
                && calls[0].starts_with("trust-settings-export -d ")
                && calls[1].starts_with("trust-settings-export ")
                && calls[2] == "find-certificate -a -p",
            "{calls:?}"
        );
        // The user's explicit distrust must override the admin's trust.
        let user_deny = export(&[(&sha1_hex(&ca), Some(3))]);
        let override_security = |args: &[&str]| -> Option<Vec<u8>> {
            match args {
                ["trust-settings-export", "-d", path] => {
                    std::fs::write(path, &admin).unwrap();
                    Some(Vec::new())
                }
                ["trust-settings-export", path] => {
                    std::fs::write(path, &user_deny).unwrap();
                    Some(Vec::new())
                }
                ["find-certificate", "-a", "-p"] => Some(keychains.clone().into_bytes()),
                _ => None,
            }
        };
        assert!(keychain_roots(&override_security).is_empty());
        // Without any trust settings the keychains are not even read.
        let none = |args: &[&str]| -> Option<Vec<u8>> {
            (args[0] == "find-certificate").then(|| keychains.clone().into_bytes())
        };
        assert!(keychain_roots(&none).is_empty());
    }

    #[test]
    fn fingerprints_are_upper_case_hex() {
        let der =
            crate::certs::pem_certificates(crate::test_certs::SELF_SIGNED_PEM.as_bytes()).remove(0);
        let hex = sha1_hex(&der);
        assert_eq!(hex.len(), 40);
        assert_eq!(
            hex,
            crate::x509::CertificateInfo::from_der(&der)
                .sha1_hex()
                .replace(':', "")
        );
        assert!(!DESCRIPTION.is_empty());
    }
}
