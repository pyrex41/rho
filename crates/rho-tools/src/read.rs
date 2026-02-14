// Read tool — file reading with line numbers, directory listing, fuzzy suggestions

use async_trait::async_trait;
use rho_core::tool::{AgentTool, ToolError};
use rho_core::types::{Content, ToolResult};
use serde_json::Value;
use std::path::Path;
use tokio_util::sync::CancellationToken;

const DEFAULT_LIMIT: usize = 2000;
const BINARY_CHECK_SIZE: usize = 8 * 1024;
const MAX_FUZZY_RESULTS: usize = 5;

pub struct ReadTool {
    working_dir: Option<std::path::PathBuf>,
}

impl ReadTool {
    pub fn new() -> Self {
        ReadTool { working_dir: None }
    }

    pub fn with_cwd(cwd: std::path::PathBuf) -> Self {
        ReadTool {
            working_dir: Some(cwd),
        }
    }

    fn resolve_path(&self, path: &str) -> std::path::PathBuf {
        let p = Path::new(path);
        if p.is_absolute() {
            p.to_path_buf()
        } else if let Some(ref cwd) = self.working_dir {
            cwd.join(p)
        } else {
            p.to_path_buf()
        }
    }
}

impl Default for ReadTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn label(&self) -> String {
        "Read File".to_string()
    }

    fn description(&self) -> String {
        "Read a file or list a directory. Returns content with line numbers.".to_string()
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The absolute path to the file or directory to read"
                },
                "offset": {
                    "type": "integer",
                    "description": "Line number to start reading from (1-indexed)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to return (default 2000)"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(
        &self,
        _tool_call_id: &str,
        params: Value,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let path_str = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::InvalidParameters("missing or invalid 'path' parameter".into())
            })?;

        let offset = params
            .get("offset")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);

        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);

        let resolved = self.resolve_path(path_str);
        let path = resolved.as_path();

        let metadata = match tokio::fs::metadata(path).await {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let message = fuzzy_suggestions(path_str).await;
                return Ok(text_result(&message));
            }
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(ToolError::ExecutionFailed(format!(
                    "Permission denied: {}",
                    path_str
                )));
            }
            Err(e) => {
                return Err(ToolError::ExecutionFailed(e.to_string()));
            }
        };

        if metadata.is_dir() {
            return read_directory(&path.to_string_lossy()).await;
        }

        // Read file
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        // Binary detection: check first 8KB for null bytes
        let check_len = bytes.len().min(BINARY_CHECK_SIZE);
        if bytes[..check_len].contains(&0) {
            return Ok(text_result(&format!("Binary file detected: {}", path_str)));
        }

        let content = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();

        let start = match offset {
            Some(o) if o >= 1 => o - 1,
            Some(_) => 0,
            None => 0,
        };

        let effective_limit = limit.unwrap_or(DEFAULT_LIMIT);
        let end = total_lines.min(start + effective_limit);

        if start >= total_lines {
            if total_lines == 0 {
                return Ok(text_result(""));
            }
            return Ok(text_result(&format!(
                "Offset {} is beyond end of file ({} lines total).",
                start + 1,
                total_lines
            )));
        }

        let selected = &lines[start..end];
        let last_line_num = start + selected.len();
        let pad_width = format!("{}", last_line_num).len();

        let mut output = String::new();
        for (i, line) in selected.iter().enumerate() {
            let line_num = start + i + 1;
            if !output.is_empty() {
                output.push('\n');
            }
            let hash = rho_hashline::compute_line_hash(line);
            output.push_str(&format!("{:>width$}:{}|{}", line_num, hash, line, width = pad_width));
        }

        // If we truncated due to default limit (no explicit limit), note remaining lines
        if limit.is_none() && end < total_lines {
            let remaining = total_lines - end;
            output.push_str(&format!(
                "\n\n[{} more lines in file. Use offset={} to continue]",
                remaining,
                end + 1,
            ));
        }

        Ok(text_result(&output))
    }
}

fn text_result(text: &str) -> ToolResult {
    ToolResult {
        content: vec![Content::Text {
            text: text.to_string(),
        }],
        details: serde_json::json!({}),
    }
}

async fn read_directory(path_str: &str) -> Result<ToolResult, ToolError> {
    let mut read_dir = tokio::fs::read_dir(path_str)
        .await
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

    let mut entries: Vec<(String, bool, std::time::SystemTime)> = Vec::new();
    while let Some(entry) = read_dir
        .next_entry()
        .await
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
    {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Ok(meta) = entry.metadata().await {
            let is_dir = meta.is_dir();
            let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            entries.push((name, is_dir, mtime));
        }
    }

    // Sort by modification time, most recent first
    entries.sort_by(|a, b| b.2.cmp(&a.2));

    let mut output = format!("Contents of {}:\n", path_str);
    for (name, is_dir, _) in &entries {
        if *is_dir {
            output.push_str(&format!("{}/\n", name));
        } else {
            output.push_str(&format!("{}\n", name));
        }
    }

    // Trim trailing newline
    if output.ends_with('\n') {
        output.pop();
    }

    Ok(text_result(&output))
}

async fn fuzzy_suggestions(path_str: &str) -> String {
    let path = Path::new(path_str);
    let parent = match path.parent() {
        Some(p) => p,
        None => return format!("File not found: {}", path_str),
    };

    let target_name = match path.file_name() {
        Some(n) => n.to_string_lossy().to_lowercase(),
        None => return format!("File not found: {}", path_str),
    };

    // Check if parent directory exists
    if tokio::fs::metadata(parent).await.is_err() {
        return format!("File not found: {}", path_str);
    }

    let mut candidates: Vec<(String, String)> = Vec::new(); // (display_path, name_lower)
    collect_candidates(parent, parent, &mut candidates, 3).await;

    let mut scored: Vec<(String, usize)> = Vec::new();
    for (display_path, name_lower) in &candidates {
        // Case-insensitive substring match
        if name_lower.contains(&target_name) || target_name.contains(name_lower.as_str()) {
            scored.push((display_path.clone(), 0));
            continue;
        }
        // Edit distance
        let dist = edit_distance(&target_name, name_lower);
        let max_len = target_name.len().max(name_lower.len());
        if max_len > 0 && dist <= max_len / 2 {
            scored.push((display_path.clone(), dist));
        }
    }

    scored.sort_by_key(|(_, dist)| *dist);
    scored.truncate(MAX_FUZZY_RESULTS);

    if scored.is_empty() {
        return format!("File not found: {}", path_str);
    }

    let mut message = format!("File not found: {}\n\nDid you mean:", path_str);
    for (suggestion, _) in &scored {
        message.push_str(&format!("\n  {}", suggestion));
    }
    message
}

async fn collect_candidates(
    base: &Path,
    dir: &Path,
    candidates: &mut Vec<(String, String)>,
    depth: usize,
) {
    if depth == 0 {
        return;
    }
    let mut read_dir = match tokio::fs::read_dir(dir).await {
        Ok(rd) => rd,
        Err(_) => return,
    };
    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        let full_path = entry.path();
        let display = full_path.to_string_lossy().to_string();
        let name_lower = name.to_lowercase();

        if let Ok(meta) = entry.metadata().await {
            if meta.is_file() {
                candidates.push((display, name_lower));
            } else if meta.is_dir() {
                // Recurse into subdirectories
                Box::pin(collect_candidates(base, &full_path, candidates, depth - 1)).await;
            }
        }
    }
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a_len = a.len();
    let b_len = b.len();
    if a_len == 0 {
        return b_len;
    }
    if b_len == 0 {
        return a_len;
    }

    let mut prev: Vec<usize> = (0..=b_len).collect();
    let mut curr: Vec<usize> = vec![0; b_len + 1];

    for (i, a_byte) in a.bytes().enumerate() {
        curr[0] = i + 1;
        for (j, b_byte) in b.bytes().enumerate() {
            let cost = if a_byte == b_byte { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1)
                .min(curr[j] + 1)
                .min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[b_len]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cancel() -> CancellationToken {
        CancellationToken::new()
    }

    fn extract_text(result: &ToolResult) -> &str {
        match &result.content[0] {
            Content::Text { text } => text.as_str(),
            _ => panic!("expected Text content"),
        }
    }

    #[tokio::test]
    async fn read_simple_file_with_line_numbers() {
        let tool = ReadTool::new();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "alpha\nbeta\ngamma\n").unwrap();

        let params = serde_json::json!({
            "path": file_path.to_str().unwrap()
        });
        let result = tool.execute("c1", params, cancel()).await.unwrap();
        let text = extract_text(&result);

        // Hashline format: LINE:HASH|content
        assert!(text.contains("|alpha"), "should contain alpha: {text}");
        assert!(text.contains("|beta"), "should contain beta: {text}");
        assert!(text.contains("|gamma"), "should contain gamma: {text}");
        // Verify hashline prefix format
        assert!(text.lines().next().unwrap().contains(":"), "should have LINE:HASH format");
    }

    #[tokio::test]
    async fn read_with_offset_and_limit() {
        let tool = ReadTool::new();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("lines.txt");
        let content: String = (1..=10).map(|i| format!("line {}\n", i)).collect();
        std::fs::write(&file_path, &content).unwrap();

        let params = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "offset": 5,
            "limit": 3
        });
        let result = tool.execute("c2", params, cancel()).await.unwrap();
        let text = extract_text(&result);

        assert!(text.contains("|line 5"), "should contain line 5: {text}");
        assert!(text.contains("|line 6"), "should contain line 6: {text}");
        assert!(text.contains("|line 7"), "should contain line 7: {text}");
        // Lines 4 and 8 should not appear
        assert!(!text.contains("|line 4"), "should not contain line 4: {text}");
        assert!(!text.contains("|line 8"), "should not contain line 8: {text}");
    }

    #[tokio::test]
    async fn read_directory_listing() {
        let tool = ReadTool::new();
        let dir = tempfile::tempdir().unwrap();

        std::fs::write(dir.path().join("file1.rs"), "fn main() {}").unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();

        let params = serde_json::json!({
            "path": dir.path().to_str().unwrap()
        });
        let result = tool.execute("c3", params, cancel()).await.unwrap();
        let text = extract_text(&result);

        assert!(text.contains("Contents of"));
        assert!(text.contains("subdir/"));
        assert!(text.contains("file1.rs"));
    }

    #[tokio::test]
    async fn detect_binary_file() {
        let tool = ReadTool::new();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("binary.bin");
        let mut data = vec![0u8; 100];
        data[0] = 0x89; // PNG-like header byte
        data[1] = 0x50;
        data[10] = 0x00; // null byte
        std::fs::write(&file_path, &data).unwrap();

        let params = serde_json::json!({
            "path": file_path.to_str().unwrap()
        });
        let result = tool.execute("c4", params, cancel()).await.unwrap();
        let text = extract_text(&result);

        assert!(text.contains("Binary file detected"));
    }

    #[tokio::test]
    async fn file_not_found_with_suggestions() {
        let tool = ReadTool::new();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "key = 1").unwrap();
        std::fs::write(dir.path().join("config.yaml"), "key: 1").unwrap();

        let bad_path = dir.path().join("confg.toml"); // typo
        let params = serde_json::json!({
            "path": bad_path.to_str().unwrap()
        });
        let result = tool.execute("c5", params, cancel()).await.unwrap();
        let text = extract_text(&result);

        assert!(text.contains("File not found"));
        assert!(text.contains("Did you mean"));
        assert!(text.contains("config.toml"));
    }

    #[tokio::test]
    async fn read_empty_file() {
        let tool = ReadTool::new();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("empty.txt");
        std::fs::write(&file_path, "").unwrap();

        let params = serde_json::json!({
            "path": file_path.to_str().unwrap()
        });
        let result = tool.execute("c6", params, cancel()).await.unwrap();
        let text = extract_text(&result);

        assert_eq!(text, "");
    }

    #[tokio::test]
    async fn large_file_default_truncation() {
        let tool = ReadTool::new();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("big.txt");
        let content: String = (1..=2500).map(|i| format!("line {}\n", i)).collect();
        std::fs::write(&file_path, &content).unwrap();

        let params = serde_json::json!({
            "path": file_path.to_str().unwrap()
        });
        let result = tool.execute("c7", params, cancel()).await.unwrap();
        let text = extract_text(&result);

        // Should show first 2000 lines and a message about remaining
        assert!(text.contains("|line 1"), "should contain line 1: first 20 chars: {}", &text[..80]);
        assert!(text.contains("|line 2000"), "should contain line 2000");
        assert!(text.contains("more lines in file"));
        assert!(text.contains("offset=2001"));
    }

    #[tokio::test]
    async fn missing_path_parameter() {
        let tool = ReadTool::new();
        let params = serde_json::json!({});
        let err = tool.execute("c8", params, cancel()).await.unwrap_err();
        match err {
            ToolError::InvalidParameters(msg) => assert!(msg.contains("path")),
            _ => panic!("expected InvalidParameters"),
        }
    }

    #[test]
    fn test_edit_distance() {
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("abc", ""), 3);
        assert_eq!(edit_distance("abc", "abc"), 0);
    }
}
