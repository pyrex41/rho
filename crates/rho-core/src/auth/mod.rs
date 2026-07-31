pub mod credentials;
pub mod grok_cli;
pub mod oauth;
pub mod xai_oauth;

pub use credentials::{OAuthCredentials, Provider};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("No credentials found. Run `rho auth login` (Anthropic) or `rho auth xai` (xAI).")]
    NoCredentials,
    #[error("ANTHROPIC_API_KEY is empty")]
    EmptyApiKey,
    #[error("Failed to read keychain: {0}")]
    KeychainError(String),
    #[error("Failed to parse keychain credentials: {0}")]
    ParseError(String),
    #[error("OAuth error: {0}")]
    OAuthError(String),
}

/// Backwards-compatible token getter for Anthropic.
///
/// Tries ANTHROPIC_API_KEY env var, then macOS Keychain (Claude Code OAuth), then file-based OAuth
/// credentials in the unified store at ~/.config/rho/credentials.json.
pub fn get_token() -> Result<String, AuthError> {
    get_token_for(Provider::Anthropic)
}

/// Resolve an access token for a specific provider.
pub fn get_token_for(provider: Provider) -> Result<String, AuthError> {
    match provider {
        Provider::Anthropic => get_anthropic_token(),
        Provider::Xai => xai_oauth::get_token(),
    }
}

fn get_anthropic_token() -> Result<String, AuthError> {
    // 1. Env var.
    match std::env::var("ANTHROPIC_API_KEY") {
        Ok(val) if val.is_empty() => return Err(AuthError::EmptyApiKey),
        Ok(val) => return Ok(val),
        Err(_) => {}
    }

    // 2. macOS Keychain (Claude Code OAuth credentials).
    if let Ok(token) = get_keychain_token() {
        return Ok(token);
    }

    // 3. Unified credentials file (migrated from the legacy path on first read).
    if let Ok(Some(creds)) = credentials::load(Provider::Anthropic) {
        if !creds.access_token.is_empty() {
            return Ok(creds.access_token);
        }
    }

    Err(AuthError::NoCredentials)
}

/// Read OAuth token from macOS Keychain where Claude Code stores credentials.
fn get_keychain_token() -> Result<String, AuthError> {
    let user = std::env::var("USER").unwrap_or_default();
    let output = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "Claude Code-credentials",
            "-a",
            &user,
            "-w",
        ])
        .output()
        .map_err(|e| AuthError::KeychainError(e.to_string()))?;

    if !output.status.success() {
        return Err(AuthError::KeychainError(
            "no credentials in keychain".into(),
        ));
    }

    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let json: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| AuthError::ParseError(e.to_string()))?;

    json.pointer("/claudeAiOauth/accessToken")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| AuthError::ParseError("missing claudeAiOauth.accessToken".into()))
}

pub fn is_oauth_token(token: &str) -> bool {
    token.starts_with("sk-ant-oat")
}

/// High-level connection state for a provider, used by the GUI's Providers tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionStatus {
    Disconnected,
    Connected {
        source: String,
        label: Option<String>,
    },
}

/// Inspect a provider's connection status without performing any network I/O.
///
/// For Anthropic: reports env var > keychain > stored OAuth credentials, in that order.
/// For xAI: reports env var > grok-CLI tokens > rho's own OAuth credentials.
pub fn connection_status(provider: Provider) -> ConnectionStatus {
    match provider {
        Provider::Anthropic => {
            if std::env::var("ANTHROPIC_API_KEY")
                .ok()
                .filter(|v| !v.is_empty())
                .is_some()
            {
                return ConnectionStatus::Connected {
                    source: "ANTHROPIC_API_KEY".into(),
                    label: None,
                };
            }
            if get_keychain_token().is_ok() {
                return ConnectionStatus::Connected {
                    source: "Claude Code keychain".into(),
                    label: None,
                };
            }
            if let Ok(Some(creds)) = credentials::load(Provider::Anthropic) {
                if !creds.access_token.is_empty() {
                    return ConnectionStatus::Connected {
                        source: "OAuth".into(),
                        label: creds.account_label,
                    };
                }
            }
            ConnectionStatus::Disconnected
        }
        Provider::Xai => {
            if std::env::var("XAI_API_KEY")
                .ok()
                .filter(|v| !v.is_empty())
                .is_some()
            {
                return ConnectionStatus::Connected {
                    source: "XAI_API_KEY".into(),
                    label: None,
                };
            }
            if let Some(creds) = grok_cli::load_xai() {
                return ConnectionStatus::Connected {
                    source: "grok CLI".into(),
                    label: creds.account_label,
                };
            }
            if let Ok(Some(creds)) = credentials::load(Provider::Xai) {
                if !creds.access_token.is_empty() {
                    return ConnectionStatus::Connected {
                        source: "OAuth".into(),
                        label: creds.account_label,
                    };
                }
            }
            ConnectionStatus::Disconnected
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn get_token_returns_value_when_set() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("ANTHROPIC_API_KEY", "sk-ant-test-key");
        let result = get_token();
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "sk-ant-test-key");
    }

    #[test]
    fn get_token_errors_when_empty() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("ANTHROPIC_API_KEY", "");
        let result = get_token();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AuthError::EmptyApiKey));
    }

    #[test]
    fn get_token_falls_back_when_env_missing() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::remove_var("ANTHROPIC_API_KEY");
        let result = get_token();
        // Will either find a keychain/file token or return NoCredentials
        match result {
            Ok(token) => assert!(!token.is_empty()),
            Err(e) => assert!(matches!(e, AuthError::NoCredentials)),
        }
    }

    #[test]
    fn is_oauth_token_recognizes_oauth() {
        assert!(is_oauth_token("sk-ant-oat-abc123"));
        assert!(is_oauth_token("sk-ant-oat"));
    }

    #[test]
    fn is_oauth_token_rejects_non_oauth() {
        assert!(!is_oauth_token("sk-ant-api-key"));
        assert!(!is_oauth_token(""));
        assert!(!is_oauth_token("some-random-token"));
    }

    #[test]
    fn connection_status_anthropic_env_var() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("ANTHROPIC_API_KEY", "sk-anything");
        let status = connection_status(Provider::Anthropic);
        match status {
            ConnectionStatus::Connected { source, .. } => assert_eq!(source, "ANTHROPIC_API_KEY"),
            _ => panic!("expected connected"),
        }
        env::remove_var("ANTHROPIC_API_KEY");
    }

    #[test]
    fn connection_status_xai_env_var() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("XAI_API_KEY", "xai-anything");
        let status = connection_status(Provider::Xai);
        match status {
            ConnectionStatus::Connected { source, .. } => assert_eq!(source, "XAI_API_KEY"),
            _ => panic!("expected connected"),
        }
        env::remove_var("XAI_API_KEY");
    }
}
