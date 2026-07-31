use async_trait::async_trait;
use rho_core::tool::{AgentTool, ToolError};
use rho_core::types::{Content, ToolResult};
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
        output.push_str(&format!(
            "\n... (diff truncated, showing first {} lines)\n",
            MAX_DIFF_LINES
        ));
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
        "Replace an exact string in a file. `old_string` must match the file content exactly \
(including whitespace and indentation). Use `replace_all: true` to replace every occurrence. \
The file must already exist — use the `write` tool to create new files."
            .to_string()
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute path to the file to edit."
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact text to find (must be unique in the file unless replace_all is true)."
                },
                "new_string": {
                    "type": "string",
                    "description": "The text to replace it with."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "If true, replace all occurrences. Default false (errors if not unique)."
                }
            },
            "required": ["file_path", "old_string", "new_string"]
        })
    }

    async fn execute(
        &self,
        _tool_call_id: &str,
        params: Value,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        // Accept both "file_path" (new) and "path" (legacy) parameter names
        let path_str = params
            .get("file_path")
            .or_else(|| params.get("path"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::InvalidParameters("missing or invalid 'file_path' parameter".into())
            })?;

        let path = self.resolve_path(path_str);

        let old_string = params
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::InvalidParameters("missing or invalid 'old_string' parameter".into())
            })?;

        let new_string = params
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::InvalidParameters("missing or invalid 'new_string' parameter".into())
            })?;

        let replace_all = params
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Read original file content
        let original = tokio::fs::read_to_string(&path).await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read {}: {}", path.display(), e))
        })?;

        // Validate match count
        let match_count = original.matches(old_string).count();
        if match_count == 0 {
            return Ok(ToolResult {
                content: vec![Content::Text {
                    text: format!(
                        "old_string not found in {}.\n\
                        Make sure the text matches exactly (including whitespace/indentation).",
                        path.display()
                    ),
                }],
                details: serde_json::json!({}),
            });
        }
        if match_count > 1 && !replace_all {
            return Ok(ToolResult {
                content: vec![Content::Text {
                    text: format!(
                        "old_string matches {} places in {}. \
                        Add more surrounding context to make it unique, \
                        or set replace_all: true to replace all occurrences.",
                        match_count,
                        path.display()
                    ),
                }],
                details: serde_json::json!({}),
            });
        }

        let result = if replace_all {
            original.replace(old_string, new_string)
        } else {
            original.replacen(old_string, new_string, 1)
        };

        // Write result back
        tokio::fs::write(&path, &result).await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to write {}: {}", path.display(), e))
        })?;

        crate::git_helpers::auto_commit_file(&path, "edit").await;

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

    #[tokio::test]
    async fn edit_basic_replace() {
        let tool = EditTool::new();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");

        std::fs::write(&file_path, "alpha\nbeta\ngamma").unwrap();

        let params = serde_json::json!({
            "file_path": file_path.to_str().unwrap(),
            "old_string": "beta",
            "new_string": "BETA"
        });

        let result = tool
            .execute("call_1", params, CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(&file_path).unwrap(),
            "alpha\nBETA\ngamma"
        );
        match &result.content[0] {
            Content::Text { text } => {
                assert!(text.contains("Edited"));
                assert!(text.contains("lines changed"));
            }
            _ => panic!("expected Text content"),
        }
    }

    #[tokio::test]
    async fn edit_multiline_replace() {
        let tool = EditTool::new();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");

        std::fs::write(&file_path, "alpha\nbeta\ngamma\ndelta").unwrap();

        let params = serde_json::json!({
            "file_path": file_path.to_str().unwrap(),
            "old_string": "beta\ngamma",
            "new_string": "new_line"
        });

        tool.execute("call_1", params, CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(&file_path).unwrap(),
            "alpha\nnew_line\ndelta"
        );
    }

    #[tokio::test]
    async fn edit_replace_all() {
        let tool = EditTool::new();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");

        std::fs::write(&file_path, "foo bar foo baz foo").unwrap();

        let params = serde_json::json!({
            "file_path": file_path.to_str().unwrap(),
            "old_string": "foo",
            "new_string": "qux",
            "replace_all": true
        });

        tool.execute("call_1", params, CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(&file_path).unwrap(),
            "qux bar qux baz qux"
        );
    }

    #[tokio::test]
    async fn edit_not_unique_without_replace_all() {
        let tool = EditTool::new();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");

        std::fs::write(&file_path, "foo foo").unwrap();

        let params = serde_json::json!({
            "file_path": file_path.to_str().unwrap(),
            "old_string": "foo",
            "new_string": "bar"
        });

        let result = tool
            .execute("call_1", params, CancellationToken::new())
            .await
            .unwrap();

        // File unchanged
        assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "foo foo");
        match &result.content[0] {
            Content::Text { text } => assert!(text.contains("matches 2 places")),
            _ => panic!("expected Text content"),
        }
    }

    #[tokio::test]
    async fn edit_old_string_not_found() {
        let tool = EditTool::new();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");

        std::fs::write(&file_path, "hello world").unwrap();

        let params = serde_json::json!({
            "file_path": file_path.to_str().unwrap(),
            "old_string": "nonexistent",
            "new_string": "something"
        });

        let result = tool
            .execute("call_1", params, CancellationToken::new())
            .await
            .unwrap();

        match &result.content[0] {
            Content::Text { text } => assert!(text.contains("not found")),
            _ => panic!("expected Text content"),
        }
    }

    #[tokio::test]
    async fn edit_file_not_found() {
        let tool = EditTool::new();
        let params = serde_json::json!({
            "file_path": "/tmp/nonexistent_rho_test_file_12345.txt",
            "old_string": "a",
            "new_string": "b"
        });

        let err = tool
            .execute("call_1", params, CancellationToken::new())
            .await
            .unwrap_err();

        match err {
            ToolError::ExecutionFailed(msg) => assert!(msg.contains("Failed to read")),
            _ => panic!("expected ExecutionFailed"),
        }
    }

    #[tokio::test]
    async fn edit_legacy_path_param() {
        // "path" should still work as an alias for "file_path"
        let tool = EditTool::new();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");

        std::fs::write(&file_path, "hello world").unwrap();

        let params = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "old_string": "hello",
            "new_string": "goodbye"
        });

        tool.execute("call_1", params, CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(&file_path).unwrap(),
            "goodbye world"
        );
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
    async fn edit_text_not_found_legacy() {
        let tool = EditTool::new();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");

        std::fs::write(&file_path, "hello world").unwrap();

        let params = serde_json::json!({
            "file_path": file_path.to_str().unwrap(),
            "old_string": "nonexistent",
            "new_string": "something"
        });

        let result = tool
            .execute("call_1", params, CancellationToken::new())
            .await
            .unwrap();

        match &result.content[0] {
            Content::Text { text } => assert!(text.contains("not found")),
            _ => panic!("expected Text content"),
        }
    }
}
