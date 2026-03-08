use std::path::Path;
use std::process::Command;

/// Attempt to auto-commit a file after a successful edit or write.
///
/// Silently skips if:
/// - The file is not inside a git repository
/// - The file is gitignored
///
/// Logs a warning (via `tracing`) if git add/commit fails, but never
/// returns an error — the caller's tool operation should still succeed.
pub fn auto_commit_file(file_path: &Path, action: &str) {
    let file_dir = match file_path.parent() {
        Some(dir) => dir,
        None => return,
    };

    // Check if inside a git work tree
    let in_git = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(file_dir)
        .output();

    match in_git {
        Ok(output) if output.status.success() => {}
        _ => return, // not a git repo — silently skip
    }

    // Check if the file is gitignored
    let check_ignore = Command::new("git")
        .args(["check-ignore", "-q"])
        .arg(file_path)
        .current_dir(file_dir)
        .output();

    if let Ok(output) = check_ignore {
        if output.status.success() {
            // File is ignored — silently skip
            return;
        }
    }

    // Extract just the filename for the commit message
    let filename = file_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");

    // git add <file>
    let add_result = Command::new("git")
        .args(["add"])
        .arg(file_path)
        .current_dir(file_dir)
        .output();

    match add_result {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("auto-commit: git add failed: {}", stderr.trim());
            return;
        }
        Err(e) => {
            tracing::warn!("auto-commit: git add failed: {}", e);
            return;
        }
    }

    // git commit -m "rho: <action> <filename>"
    let message = format!("rho: {} {}", action, filename);
    let commit_result = Command::new("git")
        .args(["commit", "-m", &message])
        .current_dir(file_dir)
        .output();

    match commit_result {
        Ok(output) if output.status.success() => {
            tracing::debug!("auto-commit: {}", message);
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("auto-commit: git commit failed: {}", stderr.trim());
        }
        Err(e) => {
            tracing::warn!("auto-commit: git commit failed: {}", e);
        }
    }
}
