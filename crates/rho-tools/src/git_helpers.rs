use std::path::Path;
use tracing::warn;

/// Auto-commit a file after a write/edit operation.
///
/// Gated by `RHO_AUTO_COMMIT=1` env var. Silently skips if:
/// - not in a git repo
/// - file is gitignored
/// - any git command fails
///
/// Never propagates errors — logs warnings on failure.
pub async fn auto_commit_file(file_path: &Path, action: &str) {
    if std::env::var("RHO_AUTO_COMMIT").as_deref() != Ok("1") {
        return;
    }

    let file_str = match file_path.to_str() {
        Some(s) => s.to_string(),
        None => return,
    };

    let dir = file_path.parent().unwrap_or(Path::new(".")).to_path_buf();

    // Check if inside a git work tree
    let output = match tokio::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(&dir)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            warn!("auto_commit: git rev-parse failed: {e}");
            return;
        }
    };
    if !output.status.success() {
        return; // not in a git repo
    }

    // Check if file is gitignored
    let ignored = tokio::process::Command::new("git")
        .args(["check-ignore", "-q", &file_str])
        .current_dir(&dir)
        .status()
        .await;
    if let Ok(status) = ignored {
        if status.success() {
            return; // file is gitignored
        }
    }

    // git add
    let add = tokio::process::Command::new("git")
        .args(["add", &file_str])
        .current_dir(&dir)
        .output()
        .await;
    if let Err(e) = add {
        warn!("auto_commit: git add failed: {e}");
        return;
    }

    // git commit
    let filename = file_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    let message = format!("rho: {action} {filename}");

    let commit = tokio::process::Command::new("git")
        .args(["commit", "-m", &message])
        .current_dir(&dir)
        .output()
        .await;
    match commit {
        Ok(o) if !o.status.success() => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            // "nothing to commit" is not an error worth warning about
            if !stderr.contains("nothing to commit") {
                warn!("auto_commit: git commit failed: {stderr}");
            }
        }
        Err(e) => {
            warn!("auto_commit: git commit failed: {e}");
        }
        _ => {}
    }
}
