use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("ANTHROPIC_API_KEY environment variable not set")]
    MissingApiKey,
    #[error("ANTHROPIC_API_KEY is empty")]
    EmptyApiKey,
}

pub fn get_token() -> Result<String, AuthError> {
    match std::env::var("ANTHROPIC_API_KEY") {
        Ok(val) if val.is_empty() => Err(AuthError::EmptyApiKey),
        Ok(val) => Ok(val),
        Err(_) => Err(AuthError::MissingApiKey),
    }
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
    fn get_token_errors_when_missing() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::remove_var("ANTHROPIC_API_KEY");
        let result = get_token();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AuthError::MissingApiKey));
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
