//! Per-server deployment, TLS and SSH preferences. Secrets are fetched by the UI worker.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    pub excluded: Vec<String>,
    pub upload_external_changes: bool,
    pub delete_remote_on_local_delete: bool,
    pub preserve_timestamps: bool,
    pub warn_newer: bool,
    pub notify_remote_changes: bool,
    /// plain, explicit (AUTH TLS), implicit (ftps://)
    pub ftp_tls: String,
    pub tls_ca_file: String,
    pub tls_client_certificate: String,
    pub passive_ftp: bool,
    pub timeout_seconds: u64,
    pub ssh_config_file: String,
    pub ssh_alias: String,
    pub jump_host: String,
    pub ssh_auth: String,
    pub accept_new_host_keys: bool,
    pub ssh_executable: String,
    pub scp_executable: String,
    pub curl_executable: String,
    pub rsync_executable: String,
    pub remote_python: String,
    pub sudo: bool,
    pub helper_directory: String,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            excluded: Vec::new(),
            upload_external_changes: false,
            delete_remote_on_local_delete: false,
            preserve_timestamps: true,
            warn_newer: true,
            notify_remote_changes: true,
            ftp_tls: "plain".into(),
            tls_ca_file: String::new(),
            tls_client_certificate: String::new(),
            passive_ftp: true,
            timeout_seconds: 300,
            ssh_config_file: String::new(),
            ssh_alias: String::new(),
            jump_host: String::new(),
            ssh_auth: "agent".into(),
            accept_new_host_keys: false,
            ssh_executable: String::new(),
            scp_executable: String::new(),
            curl_executable: String::new(),
            rsync_executable: String::new(),
            remote_python: "python3".into(),
            sudo: false,
            helper_directory: ".rustpy/helpers".into(),
        }
    }
}
impl Options {
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if !["plain", "explicit", "implicit"].contains(&self.ftp_tls.as_str()) {
            errors.push("Choose FTP, explicit FTPS or implicit FTPS".into());
        }
        if !(5..=3600).contains(&self.timeout_seconds) {
            errors.push("Transfer timeout must be between 5 and 3600 seconds".into());
        }
        if !["password", "key", "agent", "config"].contains(&self.ssh_auth.as_str()) {
            errors.push("Choose an SSH authentication method".into());
        }
        for (name, value) in [
            ("SSH config", &self.ssh_config_file),
            ("SSH alias", &self.ssh_alias),
            ("ProxyJump", &self.jump_host),
            ("Remote Python", &self.remote_python),
            ("Helpers directory", &self.helper_directory),
        ] {
            if value.contains(['\0', '\r', '\n']) {
                errors.push(format!("{name} must be a single line"));
            }
        }
        errors
    }
}
#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn old_server_options_keep_timestamps_and_require_certificate_validation() {
        let options: Options = serde_json::from_str("{}").unwrap();
        assert!(options.preserve_timestamps);
        assert!(options.warn_newer);
        assert_eq!(options.ftp_tls, "plain");
        assert!(!options.upload_external_changes);
        assert!(options.validate().is_empty());
    }
}
