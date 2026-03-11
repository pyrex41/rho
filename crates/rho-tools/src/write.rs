use async_trait::async_trait;
use rho_core::tool::{AgentTool, ToolError};
use rho_core::types::{Content, ToolResult};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

pub struct WriteTool {
    working_dir: Option<std::path::PathBuf>,
}

impl WriteTool {
    pub fn new() -> Self {
        WriteTool { working_dir: None }
    }

    pub fn with_cwd(cwd: std::path::PathBuf) -> Self {
        WriteTool {
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

impl Default for WriteTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn label(&self) -> String {
        "Write File".to_string()
    }

    fn description(&self) -> String {
        "Write content to a file at the specified path. Creates parent directories if they don't exist.".to_string()
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The absolute path to the file to write"
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file"
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(
        &self,
        _tool_call_id: &str,
        params: Value,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParameters("missing or invalid 'path' parameter".into()))?;

        let content = params
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::InvalidParameters("missing or invalid 'content' parameter".into())
            })?;

        let file_path = self.resolve_path(path);

        if let Some(parent) = file_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        }

        let bytes = content.len();
        tokio::fs::write(&file_path, content)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        crate::git_helpers::auto_commit_file(&file_path, "write").await;

        Ok(ToolResult {
            content: vec![Content::Text {
                text: format!("Successfully wrote {} bytes to {}", bytes, path),
            }],
            details: serde_json::json!({}),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[tokio::test]
    async fn write_to_temp_file() {
        let tool = WriteTool::new();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");

        let params = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "content": "hello world"
        });

        let result = tool
            .execute("call_1", params, CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(result.content.len(), 1);
        match &result.content[0] {
            Content::Text { text } => {
                assert!(text.contains("11 bytes"));
                assert!(text.contains("test.txt"));
            }
            _ => panic!("expected Text content"),
        }

        let written = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(written, "hello world");
    }

    #[tokio::test]
    async fn missing_path_parameter() {
        let tool = WriteTool::new();
        let params = serde_json::json!({
            "content": "hello"
        });

        let err = tool
            .execute("call_2", params, CancellationToken::new())
            .await
            .unwrap_err();

        match err {
            ToolError::InvalidParameters(msg) => assert!(msg.contains("path")),
            _ => panic!("expected InvalidParameters"),
        }
    }

    #[tokio::test]
    async fn creates_parent_directories() {
        let tool = WriteTool::new();
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("a").join("b").join("c").join("test.txt");

        assert!(!Path::new(dir.path().join("a").to_str().unwrap()).exists());

        let params = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "content": "nested content"
        });

        let result = tool
            .execute("call_3", params, CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(result.content.len(), 1);
        let written = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(written, "nested content");
    }
}
