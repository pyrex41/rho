//! Visible UX around llama.cpp lifecycle. The CLI must never sit silent while
//! `llama-server` loads or connects. This module wraps the provider's
//! `prepare_model_config` with:
//!
//! - a one-line `✓ Using <id> on :<port> (started <age> ago)` for cache hits
//!   (no spinner flash), and
//! - a spinner during fresh spawns that settles into
//!   `✓ Started <id> on :<port> (<elapsed>)` once `/health` is up.
//!
//! Non-llama-cpp configs pass through with no output at all.

use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use rho_core::models::{ModelConfig, ProviderType};
use rho_provider::llama_cpp::LlamaCppManager;
use std::time::{Duration, Instant, SystemTime};

/// Resolve lifecycle and rewrite `base_url` for llama-cpp models, with
/// human-visible progress on stderr. Returns the config ready for
/// `ModelRegistry::to_model` / `stream_fn_for_model`.
pub async fn prepare_with_ux(config: ModelConfig) -> Result<ModelConfig> {
    if config.provider != ProviderType::LlamaCpp {
        return Ok(config);
    }

    let id = config.id.clone();

    // Fast-path: server already up and healthy → print and return, no spinner.
    if let Some(running) = LlamaCppManager::peek(&id).await {
        let age = SystemTime::now()
            .duration_since(running.started_at)
            .unwrap_or_default();
        eprintln!(
            "\x1b[32m✓\x1b[0m Using {} on :{} (started {} ago)",
            id,
            running.port,
            humanize_age(age)
        );
        let (new_config, _ep) = rho_provider::prepare_model_config(config).await?;
        return Ok(new_config);
    }

    // Slow path: spawn (or wait for a sibling's spawn to finish health-check).
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
    );
    spinner.enable_steady_tick(Duration::from_millis(100));
    spinner.set_message(format!("Loading {} (llama.cpp)…", id));

    let start = Instant::now();
    let (new_config, endpoint) = match rho_provider::prepare_model_config(config).await {
        Ok(out) => out,
        Err(e) => {
            spinner.finish_and_clear();
            eprintln!("\x1b[31m✗\x1b[0m Failed to start {}: {:#}", id, e);
            return Err(e);
        }
    };
    spinner.finish_and_clear();

    if let Some(ep) = endpoint {
        if ep.from_cache {
            let age = SystemTime::now()
                .duration_since(ep.started_at)
                .unwrap_or_default();
            eprintln!(
                "\x1b[32m✓\x1b[0m Using {} on :{} (started {} ago)",
                id,
                ep.port,
                humanize_age(age)
            );
        } else {
            eprintln!(
                "\x1b[32m✓\x1b[0m Started {} on :{} ({:.1}s)",
                id,
                ep.port,
                start.elapsed().as_secs_f64()
            );
        }
    }
    Ok(new_config)
}

fn humanize_age(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}
