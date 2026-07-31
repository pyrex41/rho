//! Hugging Face GGUF auto-resolve.
//!
//! Given a repo like `google/gemma-4-12b-it-GGUF` and an optional quant hint
//! (e.g. `"Q4_K_M"`), picks a GGUF file from the repo and downloads it to
//! `~/.rho/models/<repo-slug>/<filename>`, with a resumable progress bar.
//!
//! If `$HF_TOKEN` is set, it is sent as a Bearer token so private / gated
//! repos work.
//!
//! Multi-shard models (files containing `-of-` in their name) are flagged
//! as unsupported with a clear error — the user should download manually
//! and point `gguf_path` at the first shard.

use anyhow::{bail, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use serde::Deserialize;
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Deserialize)]
struct RepoInfo {
    #[serde(default)]
    siblings: Vec<Sibling>,
}

#[derive(Debug, Deserialize)]
struct Sibling {
    rfilename: String,
}

/// Resolve `repo` + optional `quant_hint` to a local GGUF path, downloading
/// if necessary. Subsequent calls with the same args are cheap cache hits
/// (single HEAD to verify size, or pure lookup if already validated).
///
/// Default `quant_hint` is `Q4_K_M` — a good quality/size balance for most
/// consumer hardware.
pub async fn resolve_or_download(repo: &str, quant_hint: Option<&str>) -> Result<PathBuf> {
    let slug = repo_slug(repo);
    let cache_dir = cache_root()?.join(&slug);
    std::fs::create_dir_all(&cache_dir)
        .with_context(|| format!("mkdir {}", cache_dir.display()))?;

    let wanted_quant = quant_hint.unwrap_or("Q4_K_M");
    let client = build_client()?;

    // 1) Fast path: a file matching the quant is already cached.
    if let Some(local) = find_cached(&cache_dir, wanted_quant) {
        // Optional: could HEAD-verify size here, but a cache hit is a cache hit.
        return Ok(local);
    }

    // 2) Hit HF API to pick the file to download.
    let filename = pick_file_from_repo(&client, repo, wanted_quant).await?;

    if filename.contains("-of-") {
        bail!(
            "selected file '{}' is a multi-shard GGUF; rho doesn't auto-download \
             sharded models yet. Download the shards manually and point \
             `gguf_path` at the first one.",
            filename
        );
    }

    let local_path = cache_dir.join(&filename);
    let download_url = format!("https://huggingface.co/{}/resolve/main/{}", repo, filename);

    download_with_resume(&client, &download_url, &local_path, repo, &filename).await?;
    Ok(local_path)
}

/// Slugify a HF repo for filesystem use: `google/gemma-4-12b-it-GGUF` →
/// `google--gemma-4-12b-it-GGUF`.
fn repo_slug(repo: &str) -> String {
    repo.replace('/', "--")
}

fn cache_root() -> Result<PathBuf> {
    let home = dirs::home_dir().context("cannot locate $HOME")?;
    Ok(home.join(".rho").join("models"))
}

fn build_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        // No request timeout — GGUFs can be 8 GB and take a while.
        .user_agent(concat!("rho-provider/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

fn apply_auth(mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    if let Ok(tok) = std::env::var("HF_TOKEN") {
        if !tok.is_empty() {
            req = req.bearer_auth(tok);
        }
    }
    req
}

fn find_cached(dir: &Path, quant: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let q_lower = quant.to_lowercase();
    for entry in entries.flatten() {
        let p = entry.path();
        let name = p.file_name()?.to_str()?.to_lowercase();
        if name.ends_with(".gguf") && name.contains(&q_lower) {
            return Some(p);
        }
    }
    None
}

async fn pick_file_from_repo(
    client: &reqwest::Client,
    repo: &str,
    quant_hint: &str,
) -> Result<String> {
    let api_url = format!("https://huggingface.co/api/models/{}", repo);
    let req = apply_auth(client.get(&api_url));
    let resp = req
        .send()
        .await
        .with_context(|| format!("GET {}", api_url))?;
    if !resp.status().is_success() {
        bail!(
            "HF API {} returned {} — check the repo id and (for gated models) \
             set HF_TOKEN",
            repo,
            resp.status()
        );
    }
    let info: RepoInfo = resp.json().await.context("parse HF api response")?;

    let ggufs: Vec<&String> = info
        .siblings
        .iter()
        .map(|s| &s.rfilename)
        .filter(|f| f.to_lowercase().ends_with(".gguf"))
        .collect();

    if ggufs.is_empty() {
        bail!(
            "HF repo '{}' contains no .gguf files. Pass the GGUF-variant repo \
             (e.g. 'google/gemma-4-12b-it-GGUF', not 'google/gemma-4-12b-it').",
            repo
        );
    }

    // Prefer exact quant match. Case-insensitive substring so either
    // "gemma-4-12b-it-Q4_K_M.gguf" or "gemma-4-q4_k_m.gguf" hit.
    let q_lower = quant_hint.to_lowercase();
    if let Some(f) = ggufs.iter().find(|f| f.to_lowercase().contains(&q_lower)) {
        return Ok((*f).clone());
    }

    // Fall back: first Q4_K_M, then Q4, then first gguf.
    for fallback in ["q4_k_m", "q4_0", "q4", "q5_k_m", "q8_0"] {
        if let Some(f) = ggufs.iter().find(|f| f.to_lowercase().contains(fallback)) {
            return Ok((*f).clone());
        }
    }
    Ok((*ggufs.first().unwrap()).clone())
}

async fn download_with_resume(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    repo: &str,
    filename: &str,
) -> Result<()> {
    // HEAD for content-length (follows LFS redirect automatically).
    let head = apply_auth(client.head(url))
        .send()
        .await
        .with_context(|| format!("HEAD {}", url))?;
    if !head.status().is_success() {
        bail!("HEAD {} returned {}", url, head.status());
    }
    let total: Option<u64> = head
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok());

    let already: u64 = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
    if let Some(t) = total {
        if already == t {
            // Fully cached — leave file alone.
            return Ok(());
        }
        if already > t {
            // Local is bigger than remote — treat as corrupt, restart.
            let _ = std::fs::remove_file(dest);
        }
    }

    let mut req = client.get(url);
    if already > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={}-", already));
    }
    let req = apply_auth(req);
    let resp = req.send().await.with_context(|| format!("GET {}", url))?;
    let status = resp.status();
    if !(status.is_success() || status.as_u16() == 206) {
        bail!("GET {} returned {}", url, status);
    }

    let pb = ProgressBar::new(total.unwrap_or(0));
    pb.set_style(
        ProgressStyle::with_template(
            "{prefix:.cyan} [{bar:30.cyan/blue}] {bytes}/{total_bytes} ({eta}) {msg}",
        )
        .unwrap()
        .progress_chars("##-"),
    );
    pb.set_prefix(format!("⬇ {}/{}", repo, filename));
    pb.set_position(already);

    // Open file for writing. If we asked for a range, append from `already`;
    // otherwise truncate.
    let mut file = if already > 0 && status.as_u16() == 206 {
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(dest)
            .with_context(|| format!("open {} for resume", dest.display()))?;
        f.seek(SeekFrom::Start(already))?;
        f
    } else {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(dest)
            .with_context(|| format!("open {} for fresh write", dest.display()))?
    };

    let mut stream = resp.bytes_stream();
    use futures::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("download stream error")?;
        file.write_all(&chunk).context("write to cache file")?;
        pb.inc(chunk.len() as u64);
    }
    file.flush().ok();
    pb.finish_with_message("done");
    Ok(())
}
