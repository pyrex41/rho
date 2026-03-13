use serde::Deserialize;
use std::path::PathBuf;

const ZEN_MODELS_URL: &str = "https://opencode.ai/zen/v1/models";
const CACHE_FILE: &str = "zen-models.json";
const CACHE_TTL_SECS: u64 = 86400; // 24 hours

#[derive(Deserialize)]
struct ZenModelsResponse {
    data: Vec<ZenModel>,
}

#[derive(Deserialize)]
struct ZenModel {
    id: String,
}

/// Fetch Zen model list, using a file cache with 24h TTL.
/// Returns empty vec on any failure (network, parse, etc).
pub fn fetch_zen_models() -> Vec<String> {
    let cache_path = cache_path();

    // Try cache first
    if let Some(ids) = read_cache(&cache_path) {
        return ids;
    }

    // Blocking fetch with short timeout
    match fetch_from_api() {
        Ok(ids) => {
            write_cache(&cache_path, &ids);
            ids
        }
        Err(e) => {
            tracing::debug!("Failed to fetch Zen models: {}", e);
            vec![]
        }
    }
}

fn fetch_from_api() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()?;
    let resp: ZenModelsResponse = client.get(ZEN_MODELS_URL).send()?.json()?;
    Ok(resp.data.into_iter().map(|m| m.id).collect())
}

fn cache_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".rho")
        .join("cache")
        .join(CACHE_FILE)
}

fn read_cache(path: &PathBuf) -> Option<Vec<String>> {
    let metadata = std::fs::metadata(path).ok()?;
    let age = metadata.modified().ok()?.elapsed().ok()?;
    if age.as_secs() > CACHE_TTL_SECS {
        return None;
    }
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn write_cache(path: &PathBuf, ids: &[String]) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, serde_json::to_string(ids).unwrap_or_default());
}
