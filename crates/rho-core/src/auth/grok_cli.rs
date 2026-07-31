//! Best-effort reader for tokens written by xAI's official `grok` CLI.
//!
//! On macOS/Linux the `grok` CLI writes its tokens to `~/.grok/auth.json` keyed by
//! `"<issuer>::<client_id>"`. Shape:
//! ```json
//! {
//!   "https://auth.x.ai::b1a00492-...": {
//!     "key": "<access_token>",
//!     "refresh_token": "<refresh>",
//!     "expires_at": "2026-05-23T08:53:43.330932Z",
//!     "email": "user@example.com",
//!     ...
//!   }
//! }
//! ```
//!
//! Returns `Option<OAuthCredentials>` rather than `Result` because the entire strategy is
//! best-effort — if the file is missing, malformed, or the schema changes upstream, we
//! silently fall through to the next auth strategy.

use super::credentials::OAuthCredentials;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct GrokEntry {
    key: Option<String>,
    refresh_token: Option<String>,
    expires_at: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

fn grok_auth_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".grok")
        .join("auth.json")
}

/// Try to load xAI credentials from the official `grok` CLI's on-disk auth file.
pub fn load_xai() -> Option<OAuthCredentials> {
    let path = grok_auth_path();
    let raw = std::fs::read_to_string(&path).ok()?;
    parse_xai_from_str(&raw)
}

fn parse_xai_from_str(raw: &str) -> Option<OAuthCredentials> {
    let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(raw).ok()?;

    // Find the first entry whose key starts with "https://auth.x.ai::".
    // The xAI issuer prefix is stable; we don't pin the client_id so the entry still
    // resolves if xAI rotates the Grok-CLI client.
    let (_key, value) = map
        .into_iter()
        .find(|(k, _)| k.starts_with("https://auth.x.ai::"))?;
    let entry: GrokEntry = serde_json::from_value(value).ok()?;

    let access_token = entry.key.filter(|s| !s.is_empty())?;
    let expires_at = entry
        .expires_at
        .as_deref()
        .and_then(parse_rfc3339_to_epoch_secs);

    Some(OAuthCredentials {
        access_token,
        refresh_token: entry.refresh_token.filter(|s| !s.is_empty()),
        expires_at,
        account_label: entry.email,
    })
}

/// Parse an RFC 3339 timestamp into Unix epoch seconds. Returns None on malformed input.
fn parse_rfc3339_to_epoch_secs(s: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp())
        .and_then(|t| if t < 0 { None } else { Some(t as u64) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_grok_cli_auth_shape() {
        let raw = r#"{
            "https://auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828": {
                "key": "ey.access.token",
                "refresh_token": "rt-value",
                "expires_at": "2026-05-23T08:53:43.330932Z",
                "email": "user@example.com",
                "user_id": "abc"
            }
        }"#;
        let creds = parse_xai_from_str(raw).expect("must parse");
        assert_eq!(creds.access_token, "ey.access.token");
        assert_eq!(creds.refresh_token.as_deref(), Some("rt-value"));
        assert_eq!(creds.account_label.as_deref(), Some("user@example.com"));
        // 2026-05-23T08:53:43Z is roughly 1779173623
        let exp = creds.expires_at.expect("expires_at parsed");
        assert!(
            exp > 1_700_000_000,
            "expires_at must be sensible epoch: {exp}"
        );
    }

    #[test]
    fn returns_none_for_empty_object() {
        assert!(parse_xai_from_str("{}").is_none());
    }

    #[test]
    fn returns_none_for_malformed_json() {
        assert!(parse_xai_from_str("not json").is_none());
    }

    #[test]
    fn returns_none_when_no_xai_entry() {
        let raw = r#"{"https://auth.example.com::xyz": {"key": "tok"}}"#;
        assert!(parse_xai_from_str(raw).is_none());
    }

    #[test]
    fn skips_entry_with_empty_access_token() {
        let raw = r#"{"https://auth.x.ai::abc": {"key": ""}}"#;
        assert!(parse_xai_from_str(raw).is_none());
    }

    #[test]
    fn tolerates_missing_expires_at() {
        let raw = r#"{"https://auth.x.ai::abc": {"key": "tok"}}"#;
        let creds = parse_xai_from_str(raw).expect("must parse");
        assert_eq!(creds.access_token, "tok");
        assert!(creds.expires_at.is_none());
    }
}
