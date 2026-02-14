use async_trait::async_trait;
use rho_core::tool::{AgentTool, ToolError};
use rho_core::types::{Content, ToolResult};
use rho_hashline::{apply_hashline_edits, HashlineEdit, HashlineError};
use serde_json::Value;
use similar::{ChangeTag, TextDiff};
use tokio_util::sync::CancellationToken;

pub struct EditTool {
    working_dir: Option<std::path::PathBuf>,
}

impl EditTool {
    pub fn new() -> Self {
        EditTool { working_dir: None }
    }

    pub fn with_cwd(cwd: std::path::PathBuf) -> Self {
        EditTool {
            working_dir: Some(cwd),
        }
    }

    fn resolve_path(&self, path: &str) -> std::path::PathBuf {
        let p = std::path::Path::new(path);
        if p.is_absolute() {
            p.to_path_buf()
        } else if let Some(ref cwd) = self.working_dir {
            cwd.join(p)
        } else {
            p.to_path_buf()
        }
    }
}

impl Default for EditTool {
    fn default() -> Self {
        Self::new()
    }
}

const MAX_DIFF_LINES: usize = 50;

fn make_diff(old: &str, new: &str) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut output = String::new();
    let mut truncated = false;
    for (line_count, change) in diff.iter_all_changes().enumerate() {
        if line_count >= MAX_DIFF_LINES {
            truncated = true;
            break;
        }
        let sign = match change.tag() {
            ChangeTag::Delete => "-",
            ChangeTag::Insert => "+",
            ChangeTag::Equal => " ",
        };
        output.push_str(&format!("{}{}", sign, change));
    }
    if truncated {
        output.push_str(&format!("\n... (diff truncated, showing first {} lines)\n", MAX_DIFF_LINES));
    }
    output
}

fn count_changed_lines(old: &str, new: &str) -> usize {
    let diff = TextDiff::from_lines(old, new);
    diff.iter_all_changes()
        .filter(|c| c.tag() != ChangeTag::Equal)
        .count()
}

#[async_trait]
impl AgentTool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn label(&self) -> String {
        "Edit File".to_string()
    }

    fn description(&self) -> String {
        "Edit a file using line-addressed hashline operations or text replacement.".to_string()
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The absolute path to the file to edit"
                },
                "edits": {
                    "type": "array",
                    "description": "Array of edit operations (hashline or replace)",
                    "items": {
                        "type": "object"
                    }
                }
            },
            "required": ["path", "edits"]
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

        let path = self.resolve_path(path_str);

        let edits_value = params
            .get("edits")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                ToolError::InvalidParameters("missing or invalid 'edits' parameter".into())
            })?;

        // Read original file content
        let original = tokio::fs::read_to_string(&path).await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read {}: {}", path.display(), e))
        })?;

        // Parse edits
        let edits: Vec<HashlineEdit> = edits_value
            .iter()
            .enumerate()
            .map(|(i, v)| {
                serde_json::from_value(v.clone()).map_err(|e| {
                    ToolError::InvalidParameters(format!("edit[{}]: {}", i, e))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        // Apply edits
        let result = match apply_hashline_edits(&original, &edits) {
            Ok(new_content) => new_content,
            Err(HashlineError::Mismatch(m)) => {
                return Ok(ToolResult {
                    content: vec![Content::Text {
                        text: m.message,
                    }],
                    details: serde_json::json!({}),
                });
            }
            Err(HashlineError::TextNotFound(text)) => {
                return Ok(ToolResult {
                    content: vec![Content::Text {
                        text: format!("Text not found in file: {}", text),
                    }],
                    details: serde_json::json!({}),
                });
            }
            Err(HashlineError::MultipleMatches) => {
                return Ok(ToolResult {
                    content: vec![Content::Text {
                        text: "Multiple matches for text (expected unique match). Use 'all: true' to replace all occurrences.".to_string(),
                    }],
                    details: serde_json::json!({}),
                });
            }
            Err(e) => {
                return Err(ToolError::ExecutionFailed(e.to_string()));
            }
        };

        // Write result back
        tokio::fs::write(&path, &result).await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to write {}: {}", path.display(), e))
        })?;

        // Generate diff summary
        let diff = make_diff(&original, &result);
        let n = count_changed_lines(&original, &result);
        let summary = format!("{}\nEdited {}: {} lines changed", diff, path.display(), n);

        Ok(ToolResult {
            content: vec![Content::Text { text: summary }],
            details: serde_json::json!({}),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rho_hashline::compute_line_hash;

    fn make_content(lines: &[&str]) -> String {
        lines.join("\n")
    }

    fn hash_at(content: &str, line: usize) -> String {
        let lines: Vec<&str> = content.split('\n').collect();
        compute_line_hash(lines[line - 1]).to_string()
    }

    #[tokio::test]
    async fn edit_set_line() {
        let tool = EditTool::new();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");

        let content = make_content(&["alpha", "beta", "gamma"]);
        std::fs::write(&file_path, &content).unwrap();

        let h2 = hash_at(&content, 2);
        let params = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "set_line": { "anchor": format!("2:{}", h2), "new_text": "BETA" } }
            ]
        });

        let result = tool
            .execute("call_1", params, CancellationToken::new())
            .await
            .unwrap();

        let written = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(written, "alpha\nBETA\ngamma");

        match &result.content[0] {
            Content::Text { text } => {
                assert!(text.contains("Edited"));
                assert!(text.contains("lines changed"));
            }
            _ => panic!("expected Text content"),
        }
    }

    #[tokio::test]
    async fn edit_replace_lines() {
        let tool = EditTool::new();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");

        let content = make_content(&["alpha", "beta", "gamma", "delta"]);
        std::fs::write(&file_path, &content).unwrap();

        let h2 = hash_at(&content, 2);
        let h3 = hash_at(&content, 3);
        let params = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                {
                    "replace_lines": {
                        "start_anchor": format!("2:{}", h2),
                        "end_anchor": format!("3:{}", h3),
                        "new_text": "new_line"
                    }
                }
            ]
        });

        let result = tool
            .execute("call_1", params, CancellationToken::new())
            .await
            .unwrap();

        let written = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(written, "alpha\nnew_line\ndelta");

        match &result.content[0] {
            Content::Text { text } => {
                assert!(text.contains("Edited"));
            }
            _ => panic!("expected Text content"),
        }
    }

    #[tokio::test]
    async fn edit_insert_after() {
        let tool = EditTool::new();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");

        let content = make_content(&["alpha", "beta", "gamma"]);
        std::fs::write(&file_path, &content).unwrap();

        let h2 = hash_at(&content, 2);
        let params = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "insert_after": { "anchor": format!("2:{}", h2), "text": "inserted" } }
            ]
        });

        let result = tool
            .execute("call_1", params, CancellationToken::new())
            .await
            .unwrap();

        let written = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(written, "alpha\nbeta\ninserted\ngamma");

        match &result.content[0] {
            Content::Text { text } => {
                assert!(text.contains("Edited"));
            }
            _ => panic!("expected Text content"),
        }
    }

    #[tokio::test]
    async fn edit_replace_mode() {
        let tool = EditTool::new();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");

        let content = make_content(&["hello world", "foo bar"]);
        std::fs::write(&file_path, &content).unwrap();

        let params = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "replace": { "old_text": "hello world", "new_text": "goodbye world" } }
            ]
        });

        let result = tool
            .execute("call_1", params, CancellationToken::new())
            .await
            .unwrap();

        let written = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(written, "goodbye world\nfoo bar");

        match &result.content[0] {
            Content::Text { text } => {
                assert!(text.contains("Edited"));
            }
            _ => panic!("expected Text content"),
        }
    }

    #[tokio::test]
    async fn edit_file_not_found() {
        let tool = EditTool::new();
        let params = serde_json::json!({
            "path": "/tmp/nonexistent_rho_test_file_12345.txt",
            "edits": [
                { "replace": { "old_text": "a", "new_text": "b" } }
            ]
        });

        let err = tool
            .execute("call_1", params, CancellationToken::new())
            .await
            .unwrap_err();

        match err {
            ToolError::ExecutionFailed(msg) => {
                assert!(msg.contains("Failed to read"));
            }
            _ => panic!("expected ExecutionFailed"),
        }
    }

    #[tokio::test]
    async fn edit_mismatch_returns_helpful_message() {
        let tool = EditTool::new();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");

        let content = make_content(&["alpha", "beta", "gamma"]);
        std::fs::write(&file_path, &content).unwrap();

        let params = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "set_line": { "anchor": "2:ff", "new_text": "new" } }
            ]
        });

        let result = tool
            .execute("call_1", params, CancellationToken::new())
            .await
            .unwrap();

        // Mismatch should be returned as a successful tool result (not an error),
        // so the agent can see the correct hashes and retry
        match &result.content[0] {
            Content::Text { text } => {
                assert!(text.contains(">>>"));
                assert!(text.contains("changed since last read"));
            }
            _ => panic!("expected Text content"),
        }

        // File should NOT have been modified
        let after = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(after, content);
    }

    #[tokio::test]
    async fn edit_diff_output_format() {
        let old = "alpha\nbeta\ngamma\n";
        let new = "alpha\nBETA\ngamma\n";
        let diff = make_diff(old, new);
        assert!(diff.contains("-beta"));
        assert!(diff.contains("+BETA"));
        assert!(diff.contains(" alpha"));
    }

    #[tokio::test]
    async fn edit_text_not_found() {
        let tool = EditTool::new();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");

        std::fs::write(&file_path, "hello world").unwrap();

        let params = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "replace": { "old_text": "nonexistent", "new_text": "something" } }
            ]
        });

        let result = tool
            .execute("call_1", params, CancellationToken::new())
            .await
            .unwrap();

        match &result.content[0] {
            Content::Text { text } => {
                assert!(text.contains("Text not found"));
            }
            _ => panic!("expected Text content"),
        }
    }
}
