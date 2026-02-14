// Find tool — gitignore-aware file discovery with glob matching

use async_trait::async_trait;
use globset::Glob;
use ignore::WalkBuilder;
use rho_core::tool::{AgentTool, ToolError};
use rho_core::types::{Content, ToolResult};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tokio_util::sync::CancellationToken;

const DEFAULT_LIMIT: usize = 200;
const FIND_TIMEOUT: Duration = Duration::from_secs(5);

pub struct FindTool {
    working_dir: PathBuf,
}

impl FindTool {
    pub fn new(working_dir: PathBuf) -> Self {
        Self { working_dir }
    }
}

/// Split a glob pattern into (base_dir, glob_pattern).
///
/// The base directory is the longest literal prefix before the first glob
/// metacharacter (`*`, `?`, `[`, `{`). Examples:
///
///   "src/**/*.rs"  -> ("src",  "**/*.rs")
///   "**/*.rs"      -> (".",    "**/*.rs")
///   "Cargo.toml"   -> (".",    "Cargo.toml")
///   "src/lib.rs"   -> ("src",  "lib.rs")
fn parse_base_dir(pattern: &str) -> (&str, &str) {
    let glob_chars = ['*', '?', '[', '{'];

    let segments: Vec<&str> = pattern.split('/').collect();
    let first_glob_idx = segments
        .iter()
        .position(|seg| seg.contains(|c| glob_chars.contains(&c)));

    match first_glob_idx {
        None => {
            // No glob chars — treat as a literal path. The last segment is the
            // "pattern" and everything before it is the base dir.
            if let Some(last_slash) = pattern.rfind('/') {
                (&pattern[..last_slash], &pattern[last_slash + 1..])
            } else {
                (".", pattern)
            }
        }
        Some(0) => (".", pattern),
        Some(idx) => {
            let base_end: usize = segments[..idx].iter().map(|s| s.len() + 1).sum::<usize>() - 1;
            (&pattern[..base_end], &pattern[base_end + 1..])
        }
    }
}

struct FoundEntry {
    rel_path: String,
    is_dir: bool,
    mtime: Option<SystemTime>,
}

/// Walk `root` matching entries against `glob_pattern`, respecting .gitignore.
/// Paths in results are relative to `working_dir`.
fn walk_and_match(
    working_dir: &Path,
    root: &Path,
    glob_pattern: &str,
    limit: usize,
) -> Result<(Vec<FoundEntry>, bool), ToolError> {
    let matcher = Glob::new(glob_pattern)
        .map_err(|e| ToolError::InvalidParameters(format!("invalid glob pattern: {e}")))?
        .compile_matcher();

    let mut entries = Vec::new();
    let mut hit_limit = false;

    for result in WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .build()
    {
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };

        let abs_path = entry.path();
        let rel_to_working = match abs_path.strip_prefix(working_dir) {
            Ok(r) => r,
            Err(_) => continue,
        };

        // Convert to forward-slash string for glob matching
        let rel_str = rel_to_working.to_string_lossy();
        let rel_forward = rel_str.replace('\\', "/");

        if !matcher.is_match(&*rel_forward) {
            continue;
        }

        let is_dir = entry
            .file_type()
            .map(|ft| ft.is_dir())
            .unwrap_or(false);

        let mtime = entry.metadata().ok().and_then(|m| m.modified().ok());

        entries.push(FoundEntry {
            rel_path: rel_forward,
            is_dir,
            mtime,
        });

        if entries.len() >= limit {
            hit_limit = true;
            break;
        }
    }

    Ok((entries, hit_limit))
}

#[async_trait]
impl AgentTool for FindTool {
    fn name(&self) -> &str {
        "find"
    }

    fn label(&self) -> String {
        "Find Files".to_string()
    }

    fn description(&self) -> String {
        "Find files by glob pattern. Respects .gitignore.".to_string()
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern, e.g. '**/*.rs', 'src/**/*.ts'"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results to return (default: 200)"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(
        &self,
        _tool_call_id: &str,
        params: Value,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let pattern = params
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::InvalidParameters("missing or invalid 'pattern' parameter".into())
            })?;

        let pattern = pattern.trim();
        if pattern.is_empty() {
            return Err(ToolError::InvalidParameters(
                "pattern must not be empty".into(),
            ));
        }

        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(DEFAULT_LIMIT);

        if limit == 0 {
            return Err(ToolError::InvalidParameters(
                "limit must be a positive number".into(),
            ));
        }

        let (base_dir_str, _glob_pattern) = parse_base_dir(pattern);
        let root = self.working_dir.join(base_dir_str);

        // Full pattern for matching (relative to working_dir)
        let full_pattern = pattern.to_string();
        let working_dir = self.working_dir.clone();

        let result = tokio::time::timeout(
            FIND_TIMEOUT,
            tokio::task::spawn_blocking(move || {
                walk_and_match(&working_dir, &root, &full_pattern, limit)
            }),
        )
        .await;

        match result {
            Ok(Ok(Ok((mut entries, _hit_limit)))) => {
                if entries.is_empty() {
                    return Ok(ToolResult {
                        content: vec![Content::Text {
                            text: format!("No files found matching pattern: {pattern}"),
                        }],
                        details: serde_json::json!({}),
                    });
                }

                // Sort by modification time, most recent first.
                // Fall back to alphabetical if mtime unavailable.
                entries.sort_by(|a, b| match (&b.mtime, &a.mtime) {
                    (Some(tb), Some(ta)) => tb.cmp(ta),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => a.rel_path.cmp(&b.rel_path),
                });

                let output: Vec<String> = entries
                    .iter()
                    .map(|e| {
                        if e.is_dir {
                            format!("{}/", e.rel_path.trim_end_matches('/'))
                        } else {
                            e.rel_path.clone()
                        }
                    })
                    .collect();

                Ok(ToolResult {
                    content: vec![Content::Text {
                        text: output.join("\n"),
                    }],
                    details: serde_json::json!({}),
                })
            }
            Ok(Ok(Err(e))) => Err(e),
            Ok(Err(join_err)) => Err(ToolError::ExecutionFailed(format!(
                "find task panicked: {join_err}"
            ))),
            Err(_timeout) => {
                // Timeout — try to return a helpful message
                Err(ToolError::ExecutionFailed(
                    format!("find timed out after {}s searching for pattern: {pattern}", FIND_TIMEOUT.as_secs()),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_tool(dir: &Path) -> FindTool {
        FindTool::new(dir.to_path_buf())
    }

    fn extract_text(result: &ToolResult) -> &str {
        match &result.content[0] {
            Content::Text { text } => text.as_str(),
            _ => panic!("expected Text content"),
        }
    }

    #[test]
    fn test_parse_base_dir_with_glob() {
        assert_eq!(parse_base_dir("src/**/*.rs"), ("src", "**/*.rs"));
        assert_eq!(parse_base_dir("**/*.rs"), (".", "**/*.rs"));
        assert_eq!(parse_base_dir("Cargo.toml"), (".", "Cargo.toml"));
        assert_eq!(parse_base_dir("src/lib.rs"), ("src", "lib.rs"));
        assert_eq!(
            parse_base_dir("a/b/c/**/*.ts"),
            ("a/b/c", "**/*.ts")
        );
        assert_eq!(parse_base_dir("*.rs"), (".", "*.rs"));
    }

    #[tokio::test]
    async fn basic_glob_matching() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("main.rs"), "fn main() {}").unwrap();
        fs::write(src.join("lib.rs"), "pub mod foo;").unwrap();
        fs::write(dir.path().join("README.md"), "# Hello").unwrap();

        let tool = make_tool(dir.path());
        let params = serde_json::json!({ "pattern": "**/*.rs" });
        let result = tool
            .execute("call_1", params, CancellationToken::new())
            .await
            .unwrap();

        let text = extract_text(&result);
        assert!(text.contains("src/main.rs"), "got: {text}");
        assert!(text.contains("src/lib.rs"), "got: {text}");
        assert!(!text.contains("README.md"), "should not include .md: {text}");
    }

    #[tokio::test]
    async fn gitignore_respected() {
        let dir = tempfile::tempdir().unwrap();

        // Initialize a git repo so .gitignore is respected
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        fs::write(dir.path().join(".gitignore"), "ignored/\n").unwrap();
        let ignored = dir.path().join("ignored");
        fs::create_dir_all(&ignored).unwrap();
        fs::write(ignored.join("secret.rs"), "secret").unwrap();
        fs::write(dir.path().join("visible.rs"), "visible").unwrap();

        let tool = make_tool(dir.path());
        let params = serde_json::json!({ "pattern": "**/*.rs" });
        let result = tool
            .execute("call_1", params, CancellationToken::new())
            .await
            .unwrap();

        let text = extract_text(&result);
        assert!(text.contains("visible.rs"), "got: {text}");
        assert!(!text.contains("secret.rs"), "gitignored file should not appear: {text}");
    }

    #[tokio::test]
    async fn mtime_sorting() {
        let dir = tempfile::tempdir().unwrap();

        // Create files with different modification times
        fs::write(dir.path().join("old.rs"), "old").unwrap();
        // Small sleep to ensure different mtime
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(dir.path().join("new.rs"), "new").unwrap();

        let tool = make_tool(dir.path());
        let params = serde_json::json!({ "pattern": "*.rs" });
        let result = tool
            .execute("call_1", params, CancellationToken::new())
            .await
            .unwrap();

        let text = extract_text(&result);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        // Most recent first
        assert!(lines[0].contains("new.rs"), "newest should be first, got: {text}");
        assert!(lines[1].contains("old.rs"), "oldest should be last, got: {text}");
    }

    #[tokio::test]
    async fn limit_applied() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..10 {
            fs::write(dir.path().join(format!("file{i}.rs")), "content").unwrap();
        }

        let tool = make_tool(dir.path());
        let params = serde_json::json!({ "pattern": "*.rs", "limit": 3 });
        let result = tool
            .execute("call_1", params, CancellationToken::new())
            .await
            .unwrap();

        let text = extract_text(&result);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "limit should cap at 3, got: {text}");
    }

    #[tokio::test]
    async fn directory_entries_have_trailing_slash() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("subdir");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("file.txt"), "hi").unwrap();

        let tool = make_tool(dir.path());
        // Match directories
        let params = serde_json::json!({ "pattern": "**/subdir" });
        let result = tool
            .execute("call_1", params, CancellationToken::new())
            .await
            .unwrap();

        let text = extract_text(&result);
        assert!(
            text.contains("subdir/"),
            "directory should have trailing slash, got: {text}"
        );
    }

    #[tokio::test]
    async fn no_matches_message() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("file.txt"), "hi").unwrap();

        let tool = make_tool(dir.path());
        let params = serde_json::json!({ "pattern": "**/*.rs" });
        let result = tool
            .execute("call_1", params, CancellationToken::new())
            .await
            .unwrap();

        let text = extract_text(&result);
        assert!(
            text.contains("No files found matching pattern"),
            "got: {text}"
        );
    }

    #[tokio::test]
    async fn missing_pattern_parameter() {
        let dir = tempfile::tempdir().unwrap();
        let tool = make_tool(dir.path());
        let params = serde_json::json!({});
        let err = tool
            .execute("call_1", params, CancellationToken::new())
            .await
            .unwrap_err();

        match err {
            ToolError::InvalidParameters(msg) => assert!(msg.contains("pattern")),
            _ => panic!("expected InvalidParameters"),
        }
    }

    #[tokio::test]
    async fn empty_pattern_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let tool = make_tool(dir.path());
        let params = serde_json::json!({ "pattern": "  " });
        let err = tool
            .execute("call_1", params, CancellationToken::new())
            .await
            .unwrap_err();

        match err {
            ToolError::InvalidParameters(msg) => assert!(msg.contains("empty")),
            _ => panic!("expected InvalidParameters"),
        }
    }

    #[tokio::test]
    async fn base_dir_extraction_scopes_search() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let lib = dir.path().join("lib");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&lib).unwrap();
        fs::write(src.join("app.ts"), "app").unwrap();
        fs::write(lib.join("util.ts"), "util").unwrap();

        let tool = make_tool(dir.path());
        let params = serde_json::json!({ "pattern": "src/**/*.ts" });
        let result = tool
            .execute("call_1", params, CancellationToken::new())
            .await
            .unwrap();

        let text = extract_text(&result);
        assert!(text.contains("src/app.ts"), "got: {text}");
        assert!(!text.contains("util.ts"), "should not include lib files: {text}");
    }
}
