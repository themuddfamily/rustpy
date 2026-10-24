//! OAuth device authorization shared by account management and Git policy UI.
use super::{encode, Server};
use rustpy_core::CancellationToken;
use serde_json::Value;
use std::{
    io::Read,
    time::{Duration, Instant},
};
#[derive(Clone, Debug)]
pub struct DeviceCode {
    pub uri: String,
    pub code: String,
    pub expires_in: u64,
}
fn post(url: &str, fields: &[(&str, &str)]) -> Result<Value, String> {
    let agent = rustpy_net::agent_builder(url)
        .redirects(0)
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build();
    let body = fields
        .iter()
        .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    let response = agent
        .post(url)
        .set("Accept", "application/json")
        .set("Content-Type", "application/x-www-form-urlencoded")
        .send_string(&body)
        .map_err(|_| "Device authorization request failed".to_string())?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > 65536 {
        return Err("Device authorization response is too large".into());
    }
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}
pub fn authorize(
    server: &Server,
    client_id: &str,
    cancel: &CancellationToken,
    show: impl Fn(DeviceCode),
) -> Result<String, String> {
    if server.provider != rustpy_settings::github::Provider::GitHub {
        return Err("Use an access token for this provider".into());
    }
    if client_id.trim().is_empty() {
        return Err("Configure the OAuth application's client ID".into());
    }
    cancel.check().map_err(|error| error.to_string())?;
    let value = post(
        &format!("{}/login/device/code", server.web),
        &[
            ("client_id", client_id),
            ("scope", "repo gist read:org workflow notifications"),
        ],
    )?;
    cancel.check().map_err(|error| error.to_string())?;
    let received = Instant::now();
    let device = value["device_code"]
        .as_str()
        .ok_or("Device flow is unavailable for this OAuth application")?;
    let uri = value["verification_uri"]
        .as_str()
        .ok_or("Missing verification URL")?;
    // Device UI URLs must remain on the chosen server. Never follow a response's
    // arbitrary host while presenting an authentication prompt.
    if !uri.starts_with(&format!("{}/", server.web)) {
        return Err("Device verification URL has an unexpected origin".into());
    }
    let expires = value["expires_in"].as_u64().unwrap_or(900).min(1800);
    let mut interval = value["interval"].as_u64().unwrap_or(5).clamp(1, 60);
    let deadline = received + Duration::from_secs(expires);
    show(DeviceCode {
        uri: uri.into(),
        code: value["user_code"]
            .as_str()
            .ok_or("Missing user code")?
            .into(),
        expires_in: expires,
    });
    while Instant::now() < deadline {
        let next_poll = (Instant::now() + Duration::from_secs(interval)).min(deadline);
        while Instant::now() < next_poll {
            cancel.check().map_err(|error| error.to_string())?;
            std::thread::sleep(
                next_poll
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(50)),
            );
        }
        cancel.check().map_err(|error| error.to_string())?;
        if Instant::now() >= deadline {
            break;
        }
        let value = post(
            &format!("{}/login/oauth/access_token", server.web),
            &[
                ("client_id", client_id),
                ("device_code", device),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ],
        )?;
        cancel.check().map_err(|error| error.to_string())?;
        if Instant::now() >= deadline {
            break;
        }
        if let Some(token) = value["access_token"].as_str() {
            return Ok(token.into());
        }
        match value["error"].as_str().unwrap_or_default() {
            "authorization_pending" => {}
            "slow_down" => interval = (interval + 5).min(60),
            "access_denied" => return Err("Device authorization was declined".into()),
            "expired_token" => break,
            other => return Err(format!("Device authorization failed: {other}")),
        }
    }
    Err("Device code expired; start sign-in again".into())
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
