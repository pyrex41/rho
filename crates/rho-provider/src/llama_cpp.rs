//! Local llama.cpp server lifecycle.
//!
//! For `ProviderType::LlamaCpp` models, [`LlamaCppManager`] ensures a
//! `llama-server` subprocess is running and exposes an OpenAI-compatible
//! endpoint at `http://127.0.0.1:<port>/v1`.
//!
//! State lives in lockfiles under `~/.rho/run/<id>.lock` so spawn is shared
//! across sibling `rho-cli` invocations (e.g. the 16 agents `scud heavy` fans
//! out). An exclusive `flock` on the lockfile serializes the spawn decision;
//! waiting for `/health` happens *outside* the lock so sibling agents can all
//! converge on the same running server.
//!
//! Windows is not supported by this module (`cfg(unix)`-gated). Calling
//! [`LlamaCppManager::ensure_running`] on non-unix platforms returns an error.

use anyhow::{bail, Context, Result};
use rho_core::models::{LlamaCppOptions, ModelConfig, ProviderType};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use fs2::FileExt;

/// Result of [`LlamaCppManager::ensure_running`].
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// OpenAI-compatible root — drop-in for `ModelConfig::base_url`.
    pub base_url: String,
    /// True if we reused an already-running server.
    pub from_cache: bool,
    /// When the backing process started (wall clock).
    pub started_at: SystemTime,
    /// Port the server is bound to.
    pub port: u16,
}

/// Snapshot for `rho model ls`.
#[derive(Debug, Clone)]
pub struct RunningModel {
    pub id: String,
    pub pid: u32,
    pub port: u16,
    pub started_at: SystemTime,
    pub gguf_path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct LockFile {
    pid: u32,
    port: u16,
    gguf_path: PathBuf,
    started_at_unix: u64,
}

pub struct LlamaCppManager;

impl LlamaCppManager {
    pub fn new() -> Self {
        Self
    }

    /// Ensure a llama-server is running for `config` and return its endpoint.
    ///
    /// Acquires an exclusive flock on the per-model lockfile, checks for an
    /// already-running server, spawns one if necessary, then waits for
    /// `/health` to come up. Concurrent callers for the same model id
    /// serialize at the flock but all converge on the same port.
    pub async fn ensure_running(&self, config: &ModelConfig) -> Result<Endpoint> {
        #[cfg(not(unix))]
        {
            let _ = config;
            bail!("llama.cpp lifecycle management is only supported on Unix");
        }

        #[cfg(unix)]
        {
            if config.provider != ProviderType::LlamaCpp {
                bail!("ensure_running called on non-llama-cpp model '{}'", config.id);
            }
            let opts = config.llama_cpp.as_ref().with_context(|| {
                format!(
                    "llama-cpp model '{}' is missing [model.llama_cpp] config in ~/.rho/models.toml",
                    config.id
                )
            })?;
            let gguf = match &opts.gguf_path {
                Some(p) => p.clone(),
                None => {
                    let repo = opts.hf_repo.as_deref().with_context(|| {
                        format!(
                            "llama-cpp model '{}' needs either llama_cpp.gguf_path or llama_cpp.hf_repo",
                            config.id
                        )
                    })?;
                    let quant = opts.hf_quant.as_deref();
                    crate::hf_download::resolve_or_download(repo, quant)
                        .await
                        .with_context(|| format!("resolve HF repo {}", repo))?
                }
            };
            if !gguf.is_file() {
                bail!("gguf_path does not exist: {}", gguf.display());
            }

            let lock_dir = run_dir()?;
            std::fs::create_dir_all(&lock_dir).context("mkdir ~/.rho/run")?;
            let lock_path = lock_dir.join(format!("{}.lock", sanitize(&config.id)));

            // Critical section: lockfile-level decision. All sync, sub-ms.
            let (port, _pid, started_at, fresh_spawn) = {
                let lock_file = OpenOptions::new()
                    .create(true)
                    .read(true)
                    .write(true)
                    .open(&lock_path)
                    .context("open lockfile")?;
                lock_file.lock_exclusive().context("flock lockfile")?;

                let existing = read_lockfile(&lock_path)?;
                let result = match existing {
                    Some(lf) if pid_alive(lf.pid) => {
                        // Server exists (might still be loading — health wait happens below).
                        (lf.port, lf.pid, unix_to_system(lf.started_at_unix), false)
                    }
                    _ => {
                        // Clean slate: spawn. Stale lockfile (dead pid) is treated as no lockfile.
                        let port = pick_free_port()?;
                        let binary = find_llama_server()?;
                        let pid =
                            spawn_llama_server(&binary, &gguf, port, opts, &config.model_id)?;
                        let started = SystemTime::now();
                        write_lockfile(
                            &lock_path,
                            &LockFile {
                                pid,
                                port,
                                gguf_path: gguf.clone(),
                                started_at_unix: started
                                    .duration_since(UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs(),
                            },
                        )?;
                        (port, pid, started, true)
                    }
                };

                // Release flock early so siblings can observe the new lockfile.
                let _ = FileExt::unlock(&lock_file);
                result
            };

            // Wait for readiness. This is the long wait (model load can be 20–60s).
            wait_for_health(port, Duration::from_secs(180)).await?;

            Ok(Endpoint {
                base_url: format!("http://127.0.0.1:{}/v1", port),
                from_cache: !fresh_spawn,
                started_at,
                port,
            })
        }
    }

    /// Cheap pre-flight probe: returns Some(RunningModel) if a healthy server
    /// already serves `id`. Used by the UX layer to decide whether to show a
    /// "loading" spinner or a one-line "reusing" message.
    pub async fn peek(id: &str) -> Option<RunningModel> {
        let run = Self::list_running().ok()?;
        let model = run.into_iter().find(|m| m.id == sanitize(id))?;
        if health_check(model.port).await {
            Some(model)
        } else {
            None
        }
    }

    /// List running models — scans lockfiles, skips ones whose pid is dead.
    pub fn list_running() -> Result<Vec<RunningModel>> {
        let dir = run_dir()?;
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("lock") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some(lf) = read_lockfile(&path)? else {
                continue;
            };
            if !pid_alive(lf.pid) {
                // Leave stale lockfiles alone — ensure_running will overwrite them.
                continue;
            }
            out.push(RunningModel {
                id: stem.to_string(),
                pid: lf.pid,
                port: lf.port,
                gguf_path: lf.gguf_path,
                started_at: unix_to_system(lf.started_at_unix),
            });
        }
        Ok(out)
    }

    /// Send SIGTERM to the server for `id` and clean up its lockfile.
    /// Returns Ok(true) if a live process was signalled, Ok(false) if there
    /// was no running server for that id.
    pub fn stop(id: &str) -> Result<bool> {
        let lock_path = run_dir()?.join(format!("{}.lock", sanitize(id)));
        let Some(lf) = read_lockfile(&lock_path)? else {
            return Ok(false);
        };
        let alive = pid_alive(lf.pid);
        if alive {
            send_sigterm(lf.pid);
        }
        std::fs::remove_file(&lock_path).ok();
        Ok(alive)
    }

    /// Stop every running llama-server; returns the ids actually signalled.
    pub fn stop_all() -> Result<Vec<String>> {
        let mut stopped = Vec::new();
        for m in Self::list_running()? {
            if Self::stop(&m.id)? {
                stopped.push(m.id);
            }
        }
        Ok(stopped)
    }
}

impl Default for LlamaCppManager {
    fn default() -> Self {
        Self::new()
    }
}

fn run_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("cannot locate $HOME")?;
    Ok(home.join(".rho").join("run"))
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn unix_to_system(s: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(s)
}

fn read_lockfile(path: &Path) -> Result<Option<LockFile>> {
    if !path.is_file() {
        return Ok(None);
    }
    let mut s = String::new();
    File::open(path)
        .with_context(|| format!("open {}", path.display()))?
        .read_to_string(&mut s)?;
    if s.trim().is_empty() {
        return Ok(None);
    }
    let lf: LockFile = toml::from_str(&s)
        .with_context(|| format!("parse lockfile {}", path.display()))?;
    Ok(Some(lf))
}

fn write_lockfile(path: &Path, lf: &LockFile) -> Result<()> {
    let s = toml::to_string_pretty(lf).context("serialize lockfile")?;
    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .with_context(|| format!("write {}", path.display()))?;
    f.write_all(s.as_bytes())?;
    Ok(())
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // kill(pid, 0) returns 0 iff the process exists and we can signal it.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    false
}

#[cfg(unix)]
fn send_sigterm(pid: u32) {
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
}

#[cfg(not(unix))]
fn send_sigterm(_pid: u32) {}

fn pick_free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").context("bind 127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

fn find_llama_server() -> Result<PathBuf> {
    if let Ok(custom) = std::env::var("LLAMA_SERVER") {
        let p = PathBuf::from(&custom);
        if p.is_file() {
            return Ok(p);
        }
        bail!("LLAMA_SERVER={} is set but not a regular file", custom);
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            if dir.is_empty() {
                continue;
            }
            let candidate = Path::new(dir).join("llama-server");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    let common = [
        "/opt/homebrew/bin/llama-server",
        "/usr/local/bin/llama-server",
        "/usr/bin/llama-server",
    ];
    for c in common {
        let p = PathBuf::from(c);
        if p.is_file() {
            return Ok(p);
        }
    }
    if let Some(home) = dirs::home_dir() {
        for rel in [".cargo/bin/llama-server", ".local/bin/llama-server"] {
            let p = home.join(rel);
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    bail!(
        "could not find `llama-server` binary. Install llama.cpp (e.g. `brew install llama.cpp`) \
         or set LLAMA_SERVER=/path/to/llama-server"
    )
}

#[cfg(unix)]
fn spawn_llama_server(
    binary: &Path,
    gguf: &Path,
    port: u16,
    opts: &LlamaCppOptions,
    alias: &str,
) -> Result<u32> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let mut cmd = Command::new(binary);
    cmd.arg("-m")
        .arg(gguf)
        .arg("--port")
        .arg(port.to_string())
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--alias")
        .arg(alias);
    if let Some(ctx) = opts.ctx_size {
        cmd.arg("--ctx-size").arg(ctx.to_string());
    }
    if let Some(ngl) = opts.n_gpu_layers {
        cmd.arg("--n-gpu-layers").arg(ngl.to_string());
    }
    for extra in &opts.extra_args {
        cmd.arg(extra);
    }

    let logs_dir = run_dir()?.join("logs");
    std::fs::create_dir_all(&logs_dir).context("mkdir ~/.rho/run/logs")?;
    let stdout_log = File::create(logs_dir.join(format!("{}.out.log", sanitize(alias))))?;
    let stderr_log = File::create(logs_dir.join(format!("{}.err.log", sanitize(alias))))?;

    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(stdout_log))
        .stderr(Stdio::from(stderr_log));

    // Detach from our session so the server outlives rho-cli.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = cmd
        .spawn()
        .with_context(|| format!("spawn {}", binary.display()))?;
    Ok(child.id())
}

#[cfg(not(unix))]
fn spawn_llama_server(
    _binary: &Path,
    _gguf: &Path,
    _port: u16,
    _opts: &LlamaCppOptions,
    _alias: &str,
) -> Result<u32> {
    bail!("llama-server spawn is only implemented on Unix")
}

async fn health_check(port: u16) -> bool {
    let url = format!("http://127.0.0.1:{}/health", port);
    match reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
    {
        Ok(client) => client
            .get(&url)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false),
        Err(_) => false,
    }
}

async fn wait_for_health(port: u16, timeout: Duration) -> Result<()> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if health_check(port).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    bail!(
        "llama-server on :{} did not become healthy within {:?}",
        port,
        timeout
    )
}
