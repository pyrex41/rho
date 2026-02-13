use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("No API key found: set ANTHROPIC_API_KEY or log in with `claude`")]
    NoCredentials,
    #[error("ANTHROPIC_API_KEY is empty")]
    EmptyApiKey,
    #[error("Failed to read keychain: {0}")]
    KeychainError(String),
    #[error("Failed to parse keychain credentials: {0}")]
    ParseError(String),
}

/// Try ANTHROPIC_API_KEY env var first, then macOS Keychain OAuth token.
pub fn get_token() -> Result<String, AuthError> {
    // 1. Env var takes priority
    match std::env::var("ANTHROPIC_API_KEY") {
        Ok(val) if val.is_empty() => return Err(AuthError::EmptyApiKey),
        Ok(val) => return Ok(val),
        Err(_) => {}
    }

    // 2. Try macOS Keychain (Claude Code OAuth credentials)
    match get_keychain_token() {
        Ok(token) => Ok(token),
        Err(_) => Err(AuthError::NoCredentials),
    }
}

/// Read OAuth token from macOS Keychain where Claude Code stores credentials.
fn get_keychain_token() -> Result<String, AuthError> {
    let user = std::env::var("USER").unwrap_or_default();
    let output = std::process::Command::new("security")
        .args(["find-generic-password", "-s", "Claude Code-credentials", "-a", &user, "-w"])
        .output()
        .map_err(|e| AuthError::KeychainError(e.to_string()))?;

    if !output.status.success() {
        return Err(AuthError::KeychainError("no credentials in keychain".into()));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::Mutex;

    // Mutex to serialize env-var-dependent tests
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
        // Will either find a keychain token or return NoCredentials
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
}
