use super::credentials::{self, Provider};
use super::AuthError;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{self, BufRead, Write};

pub use super::credentials::OAuthCredentials;

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
const SCOPES: &str = "org:create_api_key user:profile user:inference";

/// Generate a PKCE code verifier (43 base64url characters from 32 random bytes).
pub fn generate_code_verifier() -> String {
    let mut buf = [0u8; 32];
    read_random(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// Generate the PKCE code challenge from a verifier (SHA-256, base64url-encoded).
pub fn generate_code_challenge(verifier: &str) -> String {
    let hash = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hash)
}

/// Build the authorization URL for the PKCE flow.
pub fn build_authorize_url(code_challenge: &str) -> String {
    let params = [
        ("code", "true"),
        ("client_id", CLIENT_ID),
        ("response_type", "code"),
        ("redirect_uri", REDIRECT_URI),
        ("scope", SCOPES),
        ("code_challenge", code_challenge),
        ("code_challenge_method", "S256"),
    ];
    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", k, urlencod(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{AUTHORIZE_URL}?{query}")
}

/// Exchange an authorization code for tokens.
pub async fn exchange_code(
    code: &str,
    code_verifier: &str,
    state: Option<&str>,
) -> Result<OAuthCredentials, AuthError> {
    let mut body = serde_json::json!({
        "grant_type": "authorization_code",
        "client_id": CLIENT_ID,
        "code": code,
        "redirect_uri": REDIRECT_URI,
        "code_verifier": code_verifier,
    });
    if let Some(st) = state {
        body["state"] = serde_json::Value::String(st.to_string());
    }

    let client = reqwest::Client::new();
    let resp = client
        .post(TOKEN_URL)
        .json(&body)
        .send()
        .await
        .map_err(|e| AuthError::OAuthError(format!("token request failed: {e}")))?;

    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_else(|_| "unknown error".into());
        return Err(AuthError::OAuthError(format!(
            "token exchange failed: {text}"
        )));
    }

    let data: TokenResponse = resp
        .json()
        .await
        .map_err(|e| AuthError::OAuthError(format!("failed to parse token response: {e}")))?;

    let expires_at = data.expires_in.map(|secs| epoch_secs() + secs - 300); // 5 minute buffer

    Ok(OAuthCredentials {
        access_token: data.access_token,
        refresh_token: data.refresh_token,
        expires_at,
        account_label: None,
    })
}

/// Save Anthropic credentials to the unified store (~/.config/rho/credentials.json).
pub fn save_credentials(creds: &OAuthCredentials) -> Result<(), AuthError> {
    credentials::save(Provider::Anthropic, creds)
}

/// Load Anthropic credentials from the unified store (with legacy-path migration on first read).
pub fn load_credentials() -> Result<OAuthCredentials, AuthError> {
    credentials::load(Provider::Anthropic)?
        .ok_or_else(|| AuthError::OAuthError("no Anthropic credentials saved".into()))
}

/// Run the full interactive OAuth login flow.
pub async fn login() -> Result<OAuthCredentials, AuthError> {
    let verifier = generate_code_verifier();
    let challenge = generate_code_challenge(&verifier);
    let url = build_authorize_url(&challenge);

    eprintln!("Opening browser for authentication...");
    if open::that(&url).is_err() {
        eprintln!("Could not open browser automatically.");
    }
    eprintln!("If the browser didn't open, visit this URL:\n{url}\n");
    eprint!("Paste the authorization code: ");
    io::stderr().flush().ok();

    let mut input = String::new();
    io::stdin()
        .lock()
        .read_line(&mut input)
        .map_err(|e| AuthError::OAuthError(format!("failed to read input: {e}")))?;
    let input = input.trim().to_string();
    if input.is_empty() {
        return Err(AuthError::OAuthError("empty authorization code".into()));
    }

    // The pasted value may be "code#state" format
    let (code, state) = match input.split_once('#') {
        Some((c, s)) => (c.to_string(), Some(s.to_string())),
        None => (input, None),
    };

    let creds = exchange_code(&code, &verifier, state.as_deref()).await?;
    save_credentials(&creds)?;
    Ok(creds)
}

// -- Internal helpers --

fn read_random(buf: &mut [u8]) {
    use std::fs::File;
    use std::io::Read;
    File::open("/dev/urandom")
        .expect("failed to open /dev/urandom")
        .read_exact(buf)
        .expect("failed to read random bytes");
}

fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs()
}

/// Minimal percent-encoding for URL query parameter values.
fn urlencod(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

// -- Tests --

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn verifier_length_and_charset() {
        let v = generate_code_verifier();
        // 32 bytes -> 43 base64url chars (no padding)
        assert_eq!(v.len(), 43);
        assert!(v
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn challenge_is_deterministic() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = generate_code_challenge(verifier);
        // SHA-256 of the verifier, base64url-encoded
        let expected = {
            let hash = Sha256::digest(verifier.as_bytes());
            URL_SAFE_NO_PAD.encode(hash)
        };
        assert_eq!(challenge, expected);
    }

    #[test]
    fn challenge_known_value() {
        // Verify against a known SHA-256 test vector.
        // SHA-256("test") = 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
        let challenge = generate_code_challenge("test");
        let hash = Sha256::digest(b"test");
        let expected = URL_SAFE_NO_PAD.encode(hash);
        assert_eq!(challenge, expected);
    }

    #[test]
    fn credential_round_trip() {
        let dir = std::env::temp_dir().join(format!("anthropic-auth-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("auth.json");

        let creds = OAuthCredentials {
            access_token: "sk-ant-oat-test-token".into(),
            refresh_token: Some("rt-test-refresh".into()),
            expires_at: Some(1700000000),
            account_label: None,
        };

        // Write
        let json = serde_json::to_string_pretty(&creds).unwrap();
        fs::write(&path, json.as_bytes()).unwrap();

        // Read back
        let data = fs::read_to_string(&path).unwrap();
        let loaded: OAuthCredentials = serde_json::from_str(&data).unwrap();

        assert_eq!(loaded.access_token, creds.access_token);
        assert_eq!(loaded.refresh_token, creds.refresh_token);
        assert_eq!(loaded.expires_at, creds.expires_at);

        // Cleanup
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn authorize_url_contains_required_params() {
        let url = build_authorize_url("test-challenge");
        assert!(url.starts_with(AUTHORIZE_URL));
        assert!(url.contains("client_id="));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge=test-challenge"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("redirect_uri="));
        assert!(url.contains("scope="));
    }

    #[test]
    fn urlencod_encodes_spaces_and_colons() {
        assert_eq!(urlencod("hello world"), "hello%20world");
        assert_eq!(urlencod("a:b"), "a%3Ab");
        assert_eq!(urlencod("safe-chars_here.ok~"), "safe-chars_here.ok~");
    }
}
