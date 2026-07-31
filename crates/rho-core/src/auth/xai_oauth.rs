//! xAI OAuth flow (PKCE w/ loopback + RFC 8628 device-code), token refresh, and inference-time
//! token resolution for the SuperGrok / X Premium subscription path.
//!
//! The OAuth parameters come from xAI's public Grok-CLI client which is the only redirect_uri
//! they will accept for desktop/loopback flows. We reuse it the same way OpenCode does — see
//! `crates/rho-core/src/auth/oauth.rs` for the equivalent Anthropic flow.

use super::credentials::{self, OAuthCredentials, Provider};
use super::oauth::{generate_code_challenge, generate_code_verifier};
use super::AuthError;
use serde::Deserialize;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// --- xAI OAuth endpoints + Grok-CLI client identifiers ---

pub const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
pub const AUTHORIZE_URL: &str = "https://auth.x.ai/oauth2/authorize";
pub const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
pub const DEVICE_AUTHORIZATION_URL: &str = "https://auth.x.ai/oauth2/device/code";
pub const DEVICE_CODE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
pub const SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";

// The redirect_uri is registered against the Grok-CLI client — the host:port must match exactly.
pub const OAUTH_HOST: &str = "127.0.0.1";
pub const OAUTH_PORT: u16 = 56121;
pub const OAUTH_REDIRECT_PATH: &str = "/callback";

/// Refresh access tokens when within this window of expiry, so a long-running chat turn doesn't
/// have to recover from a mid-flight 401.
pub const REFRESH_SKEW_SECS: u64 = 120;

const CALLBACK_TIMEOUT_SECS: u64 = 5 * 60;
const DEVICE_POLL_DEFAULT_INTERVAL_SECS: u64 = 5;
const DEVICE_POLL_MIN_INTERVAL_SECS: u64 = 1;
const DEVICE_POLL_SLOW_DOWN_INCREMENT_SECS: u64 = 5;
const DEVICE_FLOW_DEFAULT_EXPIRES_SECS: u64 = 5 * 60;

fn redirect_uri() -> String {
    format!("http://{OAUTH_HOST}:{OAUTH_PORT}{OAUTH_REDIRECT_PATH}")
}

fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs()
}

// --- Authorization URL ---

/// Build the consent screen URL for the PKCE flow.
pub fn build_authorize_url(code_challenge: &str, state: &str) -> String {
    let nonce = generate_code_verifier();
    let params = [
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", &redirect_uri()),
        ("scope", SCOPE),
        ("code_challenge", code_challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
        ("nonce", &nonce),
        // `plan=generic` opts into xAI's generic OAuth plan tier; without it accounts.x.ai rejects
        // loopback OAuth from clients that aren't on the special allowlist. `referrer=rho` is
        // best-effort attribution so xAI can distinguish rho-originated logins in their logs.
        ("plan", "generic"),
        ("referrer", "rho"),
    ];
    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", k, urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{AUTHORIZE_URL}?{query}")
}

// --- Token response shapes ---

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    id_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default)]
    interval: Option<u64>,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct OAuthError {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

fn to_credentials(resp: TokenResponse) -> OAuthCredentials {
    let expires_at = resp.expires_in.map(|s| epoch_secs() + s);
    let account_label = resp
        .id_token
        .as_deref()
        .and_then(extract_email_from_id_token);
    OAuthCredentials {
        access_token: resp.access_token,
        refresh_token: resp.refresh_token,
        expires_at,
        account_label,
    }
}

// --- PKCE flow (loopback) ---

/// Stage 1 of the loopback PKCE flow: bind the listener and produce the authorize URL.
///
/// Returns the listener (bound but not yet accepted) and the URL the user must visit. The
/// listener is bound up-front so that if the port is already in use we fail loudly *before*
/// telling the user to click anything.
pub async fn start_loopback_authorize() -> Result<LoopbackHandle, AuthError> {
    let listener = TcpListener::bind((OAUTH_HOST, OAUTH_PORT))
        .await
        .map_err(|e| {
            AuthError::OAuthError(format!(
                "failed to bind loopback listener on {OAUTH_HOST}:{OAUTH_PORT}: {e}. \
                 If xAI's grok CLI is running, stop it and retry — both share port {OAUTH_PORT}."
            ))
        })?;
    let verifier = generate_code_verifier();
    let challenge = generate_code_challenge(&verifier);
    let state = generate_code_verifier();
    let url = build_authorize_url(&challenge, &state);
    Ok(LoopbackHandle {
        listener,
        verifier,
        state,
        url,
    })
}

pub struct LoopbackHandle {
    listener: TcpListener,
    verifier: String,
    state: String,
    pub url: String,
}

impl LoopbackHandle {
    /// Stage 2: wait for the user's browser to hit the callback, validate state, and exchange the
    /// code for tokens. Times out after 5 minutes.
    pub async fn complete(self) -> Result<OAuthCredentials, AuthError> {
        let timeout = Duration::from_secs(CALLBACK_TIMEOUT_SECS);
        let (code, returned_state) = tokio::time::timeout(timeout, accept_callback(&self.listener))
            .await
            .map_err(|_| {
                AuthError::OAuthError("OAuth callback timed out after 5 minutes".into())
            })??;

        if returned_state.as_deref() != Some(self.state.as_str()) {
            return Err(AuthError::OAuthError(
                "OAuth state mismatch — possible CSRF, aborting".into(),
            ));
        }

        let resp = exchange_code(&code, &self.verifier).await?;
        let creds = to_credentials(resp);
        credentials::save(Provider::Xai, &creds)?;
        Ok(creds)
    }
}

async fn accept_callback(listener: &TcpListener) -> Result<(String, Option<String>), AuthError> {
    loop {
        let (mut socket, _addr) = listener
            .accept()
            .await
            .map_err(|e| AuthError::OAuthError(format!("accept failed: {e}")))?;

        // Read just enough of the HTTP request to get the request line. xAI redirects with the
        // code as a query parameter; we don't need headers or body.
        let mut buf = [0u8; 4096];
        let n = socket
            .read(&mut buf)
            .await
            .map_err(|e| AuthError::OAuthError(format!("read failed: {e}")))?;
        if n == 0 {
            continue;
        }

        let request = String::from_utf8_lossy(&buf[..n]);
        let first_line = request.lines().next().unwrap_or("");
        let mut parts = first_line.split_whitespace();
        let _method = parts.next();
        let target = parts.next().unwrap_or("/");

        // Parse out path and query.
        let (path, query) = match target.split_once('?') {
            Some((p, q)) => (p, q),
            None => (target, ""),
        };

        if path != OAUTH_REDIRECT_PATH {
            // Ignore stray probes like favicon.ico hits.
            let _ = socket
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                .await;
            continue;
        }

        let mut code: Option<String> = None;
        let mut state: Option<String> = None;
        let mut error: Option<String> = None;
        for pair in query.split('&') {
            let (k, v) = match pair.split_once('=') {
                Some(kv) => kv,
                None => continue,
            };
            let value = urldecode(v);
            match k {
                "code" => code = Some(value),
                "state" => state = Some(value),
                "error" => error = Some(value),
                "error_description" if error.is_none() => error = Some(value),
                _ => {}
            }
        }

        if let Some(err) = error {
            let body = html_error(&err);
            write_html(&mut socket, 200, &body).await;
            return Err(AuthError::OAuthError(format!(
                "xAI returned an error: {err}"
            )));
        }

        let Some(code) = code else {
            write_html(&mut socket, 400, &html_error("missing authorization code")).await;
            return Err(AuthError::OAuthError("missing authorization code".into()));
        };

        write_html(&mut socket, 200, HTML_SUCCESS).await;
        return Ok((code, state));
    }
}

async fn write_html(socket: &mut tokio::net::TcpStream, status: u16, body: &str) {
    let status_text = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "OK",
    };
    let response = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        status = status,
        status_text = status_text,
        len = body.len(),
        body = body,
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.shutdown().await;
}

const HTML_SUCCESS: &str = "<!doctype html><html><head><title>rho — xAI Authorized</title>\
<style>body{font-family:system-ui,sans-serif;background:#1a1b26;color:#a9b1d6;display:flex;\
align-items:center;justify-content:center;height:100vh;margin:0}\
.card{text-align:center;padding:2rem}h1{color:#9ece6a;margin-bottom:1rem}</style></head>\
<body><div class=card><h1>Authorization successful</h1>\
<p>You can close this window and return to rho.</p>\
<script>setTimeout(()=>window.close(),2000)</script></div></body></html>";

fn html_error(msg: &str) -> String {
    format!(
        "<!doctype html><html><head><title>rho — xAI Authorization Failed</title>\
<style>body{{font-family:system-ui,sans-serif;background:#1a1b26;color:#a9b1d6;display:flex;\
align-items:center;justify-content:center;height:100vh;margin:0}}\
.card{{text-align:center;padding:2rem;max-width:480px}}h1{{color:#f7768e;margin-bottom:1rem}}\
.err{{font-family:monospace;background:#2a1a1f;color:#ff917b;padding:1rem;border-radius:6px;\
margin-top:1rem}}</style></head><body><div class=card><h1>Authorization failed</h1>\
<div class=err>{}</div></div></body></html>",
        escape_html(msg),
    )
}

// --- Code exchange + refresh ---

async fn exchange_code(code: &str, code_verifier: &str) -> Result<TokenResponse, AuthError> {
    let redirect = redirect_uri();
    let body = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", &redirect),
        ("client_id", CLIENT_ID),
        ("code_verifier", code_verifier),
    ];
    post_form(TOKEN_URL, &body).await
}

/// Exchange a refresh_token for a new access_token. Returns the rotated credentials but does NOT
/// write them to disk — callers wrap that in a single-flight lock.
pub async fn refresh_tokens(refresh_token: &str) -> Result<OAuthCredentials, AuthError> {
    let body = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", CLIENT_ID),
    ];
    let resp = post_form(TOKEN_URL, &body).await?;
    Ok(to_credentials(resp))
}

async fn post_form(url: &str, body: &[(&str, &str)]) -> Result<TokenResponse, AuthError> {
    let resp = reqwest::Client::new()
        .post(url)
        .header("Accept", "application/json")
        .form(body)
        .send()
        .await
        .map_err(|e| AuthError::OAuthError(format!("token request failed: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(AuthError::OAuthError(format!(
            "xAI token endpoint returned {status}: {text}"
        )));
    }

    resp.json::<TokenResponse>()
        .await
        .map_err(|e| AuthError::OAuthError(format!("failed to parse token response: {e}")))
}

// --- Device-code (RFC 8628) flow ---

/// Stage 1 of the device-code flow: request a device_code + user_code.
pub async fn request_device_code() -> Result<DeviceFlowStart, AuthError> {
    let resp = reqwest::Client::new()
        .post(DEVICE_AUTHORIZATION_URL)
        .header("Accept", "application/json")
        .form(&[("client_id", CLIENT_ID), ("scope", SCOPE)])
        .send()
        .await
        .map_err(|e| AuthError::OAuthError(format!("device-code request failed: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(AuthError::OAuthError(format!(
            "xAI device-code endpoint returned {status}: {text}"
        )));
    }

    let parsed: DeviceCodeResponse = resp
        .json()
        .await
        .map_err(|e| AuthError::OAuthError(format!("failed to parse device-code response: {e}")))?;

    Ok(DeviceFlowStart {
        device_code: parsed.device_code,
        user_code: parsed.user_code,
        verification_uri: parsed.verification_uri,
        verification_uri_complete: parsed.verification_uri_complete,
        interval_secs: parsed
            .interval
            .unwrap_or(DEVICE_POLL_DEFAULT_INTERVAL_SECS)
            .max(DEVICE_POLL_MIN_INTERVAL_SECS),
        expires_in_secs: parsed
            .expires_in
            .unwrap_or(DEVICE_FLOW_DEFAULT_EXPIRES_SECS),
    })
}

pub struct DeviceFlowStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub interval_secs: u64,
    pub expires_in_secs: u64,
}

/// What to do after one poll iteration. Returned by `device_poll_once` so callers can drive the
/// loop themselves (useful for the GUI's `Task::perform` pipeline).
#[derive(Debug)]
pub enum DevicePollOutcome {
    Granted(OAuthCredentials),
    Pending,
    SlowDown,
    Failed(String),
}

/// Run one poll iteration of the device-code flow. Pure I/O — no sleeping.
pub async fn device_poll_once(device_code: &str) -> DevicePollOutcome {
    let body = [
        ("grant_type", DEVICE_CODE_GRANT_TYPE),
        ("device_code", device_code),
        ("client_id", CLIENT_ID),
    ];
    let resp = match reqwest::Client::new()
        .post(TOKEN_URL)
        .header("Accept", "application/json")
        .form(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return DevicePollOutcome::Failed(format!("device poll request failed: {e}")),
    };

    let status = resp.status();
    let text = match resp.text().await {
        Ok(t) => t,
        Err(e) => return DevicePollOutcome::Failed(format!("read device poll body failed: {e}")),
    };

    if status.is_success() {
        match serde_json::from_str::<TokenResponse>(&text) {
            Ok(parsed) => DevicePollOutcome::Granted(to_credentials(parsed)),
            Err(e) => DevicePollOutcome::Failed(format!("parse token response: {e}")),
        }
    } else {
        match serde_json::from_str::<OAuthError>(&text) {
            Ok(err) => match err.error.as_str() {
                "authorization_pending" => DevicePollOutcome::Pending,
                "slow_down" => DevicePollOutcome::SlowDown,
                _ => DevicePollOutcome::Failed(
                    err.error_description.unwrap_or_else(|| err.error.clone()),
                ),
            },
            Err(_) => DevicePollOutcome::Failed(format!("device poll returned {status}: {text}")),
        }
    }
}

/// Drive the full device-code flow to completion, sleeping between polls per the server's
/// interval and honouring RFC 8628 `slow_down` responses.
pub async fn run_device_flow(start: DeviceFlowStart) -> Result<OAuthCredentials, AuthError> {
    let deadline = std::time::Instant::now() + Duration::from_secs(start.expires_in_secs);
    let mut interval = Duration::from_secs(start.interval_secs);

    while std::time::Instant::now() < deadline {
        tokio::time::sleep(interval).await;
        match device_poll_once(&start.device_code).await {
            DevicePollOutcome::Granted(creds) => {
                credentials::save(Provider::Xai, &creds)?;
                return Ok(creds);
            }
            DevicePollOutcome::Pending => {}
            DevicePollOutcome::SlowDown => {
                interval += Duration::from_secs(DEVICE_POLL_SLOW_DOWN_INCREMENT_SECS);
            }
            DevicePollOutcome::Failed(msg) => {
                return Err(AuthError::OAuthError(format!("device flow failed: {msg}")));
            }
        }
    }
    Err(AuthError::OAuthError(
        "device flow expired before authorization completed".into(),
    ))
}

// --- Token resolution for inference time ---

/// Return a usable xAI access token, refreshing if it's within REFRESH_SKEW_SECS of expiry.
///
/// Resolution order:
/// 1. xAI tokens written by the official `grok` CLI (~/.grok/auth.json) — read-only, never persisted.
/// 2. xAI tokens written by rho's own OAuth flow (~/.config/rho/credentials.json) — refreshed in place.
pub fn get_token() -> Result<String, AuthError> {
    if let Some(creds) = super::grok_cli::load_xai() {
        if !is_expired(&creds) {
            return Ok(creds.access_token);
        }
        // grok CLI tokens have a refresh_token but we don't write back to their file; if the
        // grok CLI token is expired we fall through to rho's own credentials.
    }

    let Some(mut creds) = credentials::load(Provider::Xai)? else {
        return Err(AuthError::NoCredentials);
    };

    if should_refresh(&creds) {
        if let Some(ref rt) = creds.refresh_token {
            let rt = rt.clone();
            // Block on the refresh so resolve_api_key (sync) gets a fresh token. We hop onto a
            // dedicated runtime if we're outside one; inside a runtime we use block_in_place.
            let refreshed = run_blocking(async move { refresh_tokens(&rt).await });
            match refreshed {
                Ok(new_creds) => {
                    let preserved_refresh = new_creds
                        .refresh_token
                        .clone()
                        .or(creds.refresh_token.clone());
                    let preserved_label = new_creds
                        .account_label
                        .clone()
                        .or(creds.account_label.clone());
                    let new_creds = OAuthCredentials {
                        access_token: new_creds.access_token,
                        refresh_token: preserved_refresh,
                        expires_at: new_creds.expires_at,
                        account_label: preserved_label,
                    };
                    credentials::save(Provider::Xai, &new_creds)?;
                    creds = new_creds;
                    tracing::info!("xAI access token refreshed");
                }
                Err(e) => {
                    tracing::warn!("xAI token refresh failed, falling back to stored token: {e}");
                }
            }
        }
    }

    Ok(creds.access_token)
}

fn is_expired(creds: &OAuthCredentials) -> bool {
    creds
        .expires_at
        .map(|exp| epoch_secs() >= exp)
        .unwrap_or(false)
}

fn should_refresh(creds: &OAuthCredentials) -> bool {
    match creds.expires_at {
        Some(exp) => epoch_secs() + REFRESH_SKEW_SECS >= exp,
        None => false,
    }
}

fn run_blocking<F, T>(future: F) -> T
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    // Inside a tokio runtime: use a fresh runtime in a blocking thread so we never deadlock the
    // current worker. Outside a runtime: spin up an ephemeral runtime on this thread.
    match tokio::runtime::Handle::try_current() {
        Ok(_) => std::thread::scope(|s| {
            s.spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("create blocking runtime")
                    .block_on(future)
            })
            .join()
            .expect("blocking refresh thread")
        }),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("create blocking runtime")
            .block_on(future),
    }
}

// --- helpers ---

fn extract_email_from_id_token(id_token: &str) -> Option<String> {
    // id_token is a JWT — header.payload.signature. We only need the payload's `email` claim and
    // never make a trust decision from it, so unsigned decode is safe.
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;

    let mut parts = id_token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    // Pad to a multiple of 4 for the standard engine; URL_SAFE_NO_PAD already tolerates either.
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')))
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    claims
        .get("email")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn urldecode(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_contains_required_params() {
        let url = build_authorize_url("chall-xyz", "state-abc");
        assert!(url.starts_with(AUTHORIZE_URL));
        assert!(url.contains("client_id=b1a00492-073a-47ea-816f-4c329264a828"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge=chall-xyz"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=state-abc"));
        assert!(url.contains("plan=generic"));
        assert!(url.contains("scope=openid"));
        // redirect_uri must be percent-encoded
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A56121%2Fcallback"));
    }

    #[test]
    fn urlencode_handles_specials() {
        assert_eq!(urlencode("a b"), "a%20b");
        assert_eq!(urlencode("a:b/c"), "a%3Ab%2Fc");
        assert_eq!(urlencode("safe-chars_here.ok~"), "safe-chars_here.ok~");
    }

    #[test]
    fn urldecode_handles_specials() {
        assert_eq!(urldecode("a%20b"), "a b");
        assert_eq!(urldecode("a+b"), "a b");
        assert_eq!(urldecode("a%3Ab%2Fc"), "a:b/c");
        // Malformed percent-escapes should be preserved, not panic.
        assert_eq!(urldecode("a%ZZb"), "a%ZZb");
    }

    #[test]
    fn extract_email_from_jwt() {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;
        let payload = serde_json::json!({"email": "u@example.com", "sub": "123"});
        let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
        let id_token = format!("eyJhbGciOiJSUzI1NiJ9.{payload_b64}.signature");
        assert_eq!(
            extract_email_from_id_token(&id_token).as_deref(),
            Some("u@example.com")
        );
    }

    #[test]
    fn extract_email_handles_garbage() {
        assert!(extract_email_from_id_token("not.a.jwt").is_none());
        assert!(extract_email_from_id_token("only.two").is_none());
        assert!(extract_email_from_id_token("").is_none());
    }

    #[test]
    fn is_expired_unset_means_not_expired() {
        let creds = OAuthCredentials {
            access_token: "t".into(),
            ..Default::default()
        };
        assert!(!is_expired(&creds));
    }

    #[test]
    fn should_refresh_within_skew() {
        let creds = OAuthCredentials {
            access_token: "t".into(),
            expires_at: Some(epoch_secs() + 60),
            ..Default::default()
        };
        assert!(should_refresh(&creds));
    }

    #[test]
    fn should_not_refresh_when_fresh() {
        let creds = OAuthCredentials {
            access_token: "t".into(),
            expires_at: Some(epoch_secs() + 3600),
            ..Default::default()
        };
        assert!(!should_refresh(&creds));
    }

    #[test]
    fn html_error_escapes_input() {
        let body = html_error("<script>alert(1)</script>");
        assert!(body.contains("&lt;script&gt;"));
        assert!(!body.contains("<script>alert"));
    }

    /// Round-trip the callback parser: bind a listener on a dynamic port, fake a browser
    /// hitting `/callback?code=...&state=...`, confirm we extract both fields and reply
    /// with HTTP 200.
    #[tokio::test]
    async fn loopback_callback_round_trip() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move { accept_callback(&listener).await });

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let req = b"GET /callback?code=abc123&state=xyz HTTP/1.1\r\nHost: localhost\r\n\r\n";
        stream.write_all(req).await.unwrap();
        let mut buf = Vec::new();
        let _ = stream.read_to_end(&mut buf).await;
        let response = String::from_utf8_lossy(&buf);
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.contains("Authorization successful"));

        let (code, state) = server.await.unwrap().unwrap();
        assert_eq!(code, "abc123");
        assert_eq!(state.as_deref(), Some("xyz"));
    }

    #[tokio::test]
    async fn loopback_callback_surfaces_error_param() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move { accept_callback(&listener).await });

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let req = b"GET /callback?error=access_denied HTTP/1.1\r\nHost: localhost\r\n\r\n";
        stream.write_all(req).await.unwrap();
        let mut buf = Vec::new();
        let _ = stream.read_to_end(&mut buf).await;
        // The server still writes a 200 page so the user sees an error UI in the browser,
        // but the function call returns Err.
        let result = server.await.unwrap();
        assert!(result.is_err(), "error param must surface as Err");
    }
}
