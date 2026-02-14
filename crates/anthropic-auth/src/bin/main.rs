use anthropic_auth::{get_token, is_oauth_token, oauth, AuthError};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "anthropic-auth", about = "Anthropic authentication manager")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Log in via OAuth (opens browser)
    Login,
    /// Print the current API token
    Token,
    /// Show authentication status
    Status,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::Login => cmd_login().await,
        Commands::Token => cmd_token(),
        Commands::Status => cmd_status(),
    }
}

async fn cmd_login() {
    match oauth::login().await {
        Ok(_) => eprintln!("Login successful."),
        Err(e) => {
            eprintln!("Login failed: {e}");
            std::process::exit(1);
        }
    }
}

fn cmd_token() {
    match get_token() {
        Ok(token) => print!("{token}"),
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

fn cmd_status() {
    // Check env var
    if let Ok(val) = std::env::var("ANTHROPIC_API_KEY") {
        if !val.is_empty() {
            if is_oauth_token(&val) {
                println!("Authenticated via ANTHROPIC_API_KEY (OAuth token)");
            } else {
                println!("Authenticated via ANTHROPIC_API_KEY");
            }
            return;
        }
    }

    // Check keychain
    if let Ok(token) = get_token_source_keychain() {
        if is_oauth_token(&token) {
            println!("Authenticated via macOS Keychain (OAuth token)");
        } else {
            println!("Authenticated via macOS Keychain");
        }
        return;
    }

    // Check file credentials
    if let Ok(creds) = oauth::load_credentials() {
        if !creds.access_token.is_empty() {
            let expired = creds
                .expires_at
                .map(|exp| {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    now >= exp
                })
                .unwrap_or(false);
            if expired {
                println!("Authenticated via OAuth file credentials (token expired)");
            } else {
                println!("Authenticated via OAuth file credentials");
            }
            return;
        }
    }

    println!("Not authenticated");
}

/// Try reading a token directly from the keychain (bypassing env var check).
fn get_token_source_keychain() -> Result<String, AuthError> {
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
