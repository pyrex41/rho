use super::AuthError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// OAuth credentials shared across providers.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OAuthCredentials {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Unix epoch seconds when access_token expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    /// Optional account identifier (email) extracted from id_token, for UI display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_label: Option<String>,
}

/// Identifier for a provider that uses OAuth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    Xai,
}

impl Provider {
    pub fn key(&self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
            Provider::Xai => "xai",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CredentialsFile {
    #[serde(flatten)]
    pub providers: BTreeMap<String, OAuthCredentials>,
}

/// Path to the unified credentials file: ~/.config/rho/credentials.json.
pub fn credentials_path() -> PathBuf {
    let config = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    config.join("rho").join("credentials.json")
}

/// Path to the legacy Anthropic-only credentials file.
pub fn legacy_anthropic_path() -> PathBuf {
    let config = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    config.join("anthropic-auth").join("auth.json")
}

/// Load the unified credentials file, migrating from the legacy Anthropic path on first read.
///
/// Returns an empty file (not an error) if neither file exists, so callers can layer this on
/// top of other auth strategies cleanly.
pub fn load_all() -> Result<CredentialsFile, AuthError> {
    let path = credentials_path();
    if path.exists() {
        let data = std::fs::read_to_string(&path)
            .map_err(|e| AuthError::OAuthError(format!("failed to read credentials: {e}")))?;
        let file: CredentialsFile = serde_json::from_str(&data)
            .map_err(|e| AuthError::OAuthError(format!("failed to parse credentials: {e}")))?;
        return Ok(file);
    }

    // First-read migration from legacy file.
    let legacy = legacy_anthropic_path();
    if legacy.exists() {
        if let Ok(data) = std::fs::read_to_string(&legacy) {
            if let Ok(old) = serde_json::from_str::<OAuthCredentials>(&data) {
                let mut file = CredentialsFile::default();
                file.providers
                    .insert(Provider::Anthropic.key().to_string(), old);
                // Best-effort write; ignore failure so a read-only home dir still works.
                let _ = save_all(&file);
                return Ok(file);
            }
        }
    }

    Ok(CredentialsFile::default())
}

/// Read the credentials for a single provider.
pub fn load(provider: Provider) -> Result<Option<OAuthCredentials>, AuthError> {
    let file = load_all()?;
    Ok(file.providers.get(provider.key()).cloned())
}

/// Write a provider's credentials, preserving entries for other providers.
pub fn save(provider: Provider, creds: &OAuthCredentials) -> Result<(), AuthError> {
    let mut file = load_all().unwrap_or_default();
    file.providers
        .insert(provider.key().to_string(), creds.clone());
    save_all(&file)
}

/// Remove a provider's credentials. No-op if absent.
pub fn delete(provider: Provider) -> Result<(), AuthError> {
    let mut file = load_all().unwrap_or_default();
    if file.providers.remove(provider.key()).is_some() {
        save_all(&file)?;
    }
    Ok(())
}

fn save_all(file: &CredentialsFile) -> Result<(), AuthError> {
    let path = credentials_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AuthError::OAuthError(format!("failed to create config dir: {e}")))?;
    }
    let json = serde_json::to_string_pretty(file)
        .map_err(|e| AuthError::OAuthError(format!("failed to serialize credentials: {e}")))?;
    std::fs::write(&path, json.as_bytes())
        .map_err(|e| AuthError::OAuthError(format!("failed to write credentials: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&path, perms)
            .map_err(|e| AuthError::OAuthError(format!("failed to set permissions: {e}")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "rho-credentials-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    #[test]
    fn serializes_with_provider_keys() {
        let mut file = CredentialsFile::default();
        file.providers.insert(
            "anthropic".into(),
            OAuthCredentials {
                access_token: "a".into(),
                ..Default::default()
            },
        );
        file.providers.insert(
            "xai".into(),
            OAuthCredentials {
                access_token: "x".into(),
                ..Default::default()
            },
        );
        let json = serde_json::to_string(&file).unwrap();
        assert!(json.contains("\"anthropic\""));
        assert!(json.contains("\"xai\""));
    }

    #[test]
    fn round_trip_via_disk() {
        // We can't safely override credentials_path() globally; round-trip the serde shape.
        let creds = OAuthCredentials {
            access_token: "tok".into(),
            refresh_token: Some("rt".into()),
            expires_at: Some(1_700_000_000),
            account_label: Some("user@example.com".into()),
        };
        let mut file = CredentialsFile::default();
        file.providers.insert("xai".into(), creds.clone());

        let dir = test_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("credentials.json");
        fs::write(&path, serde_json::to_string_pretty(&file).unwrap()).unwrap();

        let read: CredentialsFile = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let got = read.providers.get("xai").unwrap();
        assert_eq!(got.access_token, "tok");
        assert_eq!(got.refresh_token.as_deref(), Some("rt"));
        assert_eq!(got.expires_at, Some(1_700_000_000));
        assert_eq!(got.account_label.as_deref(), Some("user@example.com"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_fields_default() {
        let json = r#"{"xai":{"access_token":"only"}}"#;
        let file: CredentialsFile = serde_json::from_str(json).unwrap();
        let xai = file.providers.get("xai").unwrap();
        assert_eq!(xai.access_token, "only");
        assert!(xai.refresh_token.is_none());
        assert!(xai.expires_at.is_none());
    }

    #[test]
    fn legacy_anthropic_shape_parses_as_oauth_credentials() {
        // The legacy file is just OAuthCredentials directly, not wrapped.
        let legacy = r#"{"access_token":"sk-ant-oat-x","refresh_token":"rt","expires_at":123}"#;
        let parsed: OAuthCredentials = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.access_token, "sk-ant-oat-x");
        assert_eq!(parsed.refresh_token.as_deref(), Some("rt"));
        assert_eq!(parsed.expires_at, Some(123));
    }
}
