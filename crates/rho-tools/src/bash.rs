// Bash tool — PTY command execution with timeout and output truncation

use async_trait::async_trait;
use portable_pty::{CommandBuilder, PtySize};
use rho_core::tool::{AgentTool, ToolError};
use rho_core::types::{Content, ToolResult};
use serde_json::Value;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

const DEFAULT_TIMEOUT_SECS: u64 = 300;
const MAX_TIMEOUT_SECS: u64 = 3600;
const MAX_OUTPUT_BYTES: usize = 102_400; // 100KB
const TRUNCATION_EDGE: usize = 10_240; // 10KB kept at each end

pub struct BashTool {
    working_dir: PathBuf,
}

impl BashTool {
    pub fn new(working_dir: PathBuf) -> Self {
        Self { working_dir }
    }
}

/// Truncate output that exceeds MAX_OUTPUT_BYTES, keeping the first and last
/// TRUNCATION_EDGE bytes with a marker in the middle.
fn truncate_output(output: &str) -> String {
    if output.len() <= MAX_OUTPUT_BYTES {
        return output.to_string();
    }
    let total = output.len();

    // Find a valid UTF-8 boundary for the head slice
    let head_end = {
        let mut end = TRUNCATION_EDGE;
        while end > 0 && !output.is_char_boundary(end) {
            end -= 1;
        }
        end
    };

    // Find a valid UTF-8 boundary for the tail slice
    let tail_start = {
        let mut start = total.saturating_sub(TRUNCATION_EDGE);
        while start < total && !output.is_char_boundary(start) {
            start += 1;
        }
        start
    };

    format!(
        "{}\n\n[...output truncated ({} bytes total)...]\n\n{}",
        &output[..head_end],
        total,
        &output[tail_start..],
    )
}

#[async_trait]
impl AgentTool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn label(&self) -> String {
        "Bash".to_string()
    }

    fn description(&self) -> String {
        "Execute a shell command and return stdout/stderr.".to_string()
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command to execute"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default 300, max 3600)"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(
        &self,
        _tool_call_id: &str,
        params: Value,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let command = params
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::InvalidParameters("missing or invalid 'command' parameter".into())
            })?;

        let timeout_secs = params
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .min(MAX_TIMEOUT_SECS);

        // Set up the PTY
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(PtySize::default())
            .map_err(|e| ToolError::ExecutionFailed(format!("failed to open pty: {e}")))?;

        let mut cmd = CommandBuilder::new("bash");
        cmd.arg("-c");
        cmd.arg(command);
        cmd.cwd(&self.working_dir);

        // Spawn the child process
        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| ToolError::ExecutionFailed(format!("failed to spawn command: {e}")))?;

        // Drop slave so the master reader gets EOF when the child exits
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| ToolError::ExecutionFailed(format!("failed to clone pty reader: {e}")))?;

        // Clone a killer handle so we can kill the child on timeout
        let killer = child.clone_killer();

        // Read output in a blocking thread (PTY reader is blocking I/O)
        let read_handle = tokio::task::spawn_blocking(move || {
            let mut buf = Vec::new();
            let _ = std::io::Read::read_to_end(&mut reader, &mut buf);
            buf
        });

        // Wait for the child in a blocking thread
        let wait_handle = tokio::task::spawn_blocking(move || child.wait());

        // Apply timeout to both operations
        let timeout_duration = std::time::Duration::from_secs(timeout_secs);
        match tokio::time::timeout(timeout_duration, async {
            let output_bytes = read_handle
                .await
                .map_err(|e| ToolError::ExecutionFailed(format!("read task panicked: {e}")))?;
            let exit_status = wait_handle
                .await
                .map_err(|e| ToolError::ExecutionFailed(format!("wait task panicked: {e}")))?
                .map_err(|e| ToolError::ExecutionFailed(format!("failed to wait on child: {e}")))?;
            Ok::<_, ToolError>((output_bytes, exit_status))
        })
        .await
        {
            Ok(Ok((output_bytes, exit_status))) => {
                let output = String::from_utf8_lossy(&output_bytes);
                let output = truncate_output(&output);
                let exit_code = exit_status.exit_code();

                if exit_code == 0 {
                    Ok(ToolResult {
                        content: vec![Content::Text { text: output }],
                        details: serde_json::json!({"exit_code": exit_code}),
                    })
                } else {
                    Ok(ToolResult {
                        content: vec![Content::Text {
                            text: format!("{output}\n\nExit code: {exit_code}"),
                        }],
                        details: serde_json::json!({"exit_code": exit_code}),
                    })
                }
            }
            Ok(Err(e)) => Err(e),
            Err(_) => {
                // Timeout: kill the child process
                let mut killer = killer;
                let _ = killer.kill();

                // Try to collect any partial output that was read
                let partial = "(timeout — command did not complete)";

                Ok(ToolResult {
                    content: vec![Content::Text {
                        text: format!(
                            "Command timed out after {timeout_secs} seconds.\n{partial}"
                        ),
                    }],
                    details: serde_json::json!({"timeout": true}),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn tool_in(dir: &std::path::Path) -> BashTool {
        BashTool::new(dir.to_path_buf())
    }

    fn cancel() -> CancellationToken {
        CancellationToken::new()
    }

    fn text_of(result: &ToolResult) -> &str {
        match &result.content[0] {
            Content::Text { text } => text.as_str(),
            _ => panic!("expected Text content"),
        }
    }

    #[tokio::test]
    async fn simple_echo() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_in(dir.path());
        let params = serde_json::json!({"command": "echo hello"});
        let result = tool.execute("c1", params, cancel()).await.unwrap();
        let output = text_of(&result);
        assert!(
            output.contains("hello"),
            "expected 'hello' in output: {output}"
        );
        assert_eq!(result.details["exit_code"], 0);
    }

    #[tokio::test]
    async fn working_directory_respected() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_in(dir.path());
        let params = serde_json::json!({"command": "pwd"});
        let result = tool.execute("c2", params, cancel()).await.unwrap();
        let output = text_of(&result);
        // On macOS, /tmp -> /private/tmp, so canonicalize both
        let expected = dir.path().canonicalize().unwrap();
        let actual_trimmed = output.trim();
        let actual = std::path::Path::new(actual_trimmed)
            .canonicalize()
            .unwrap_or_else(|_| std::path::PathBuf::from(actual_trimmed));
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn nonzero_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_in(dir.path());
        let params = serde_json::json!({"command": "exit 42"});
        let result = tool.execute("c3", params, cancel()).await.unwrap();
        assert_eq!(result.details["exit_code"], 42);
        let output = text_of(&result);
        assert!(
            output.contains("Exit code: 42"),
            "expected exit code info: {output}"
        );
    }

    #[tokio::test]
    async fn output_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_in(dir.path());
        // Generate output well over 100KB
        let params = serde_json::json!({"command": "seq 1 100000"});
        let result = tool.execute("c4", params, cancel()).await.unwrap();
        let output = text_of(&result);
        assert!(
            output.contains("[...output truncated"),
            "expected truncation marker: (output len = {})",
            output.len()
        );
        // The truncated output should be around 20KB + marker, well under 100KB
        assert!(output.len() < MAX_OUTPUT_BYTES);
    }

    #[tokio::test]
    async fn timeout() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_in(dir.path());
        let params = serde_json::json!({"command": "sleep 30", "timeout": 1});
        let start = Instant::now();
        let result = tool.execute("c5", params, cancel()).await.unwrap();
        let elapsed = start.elapsed();
        let output = text_of(&result);
        assert!(
            output.contains("timed out"),
            "expected timeout message: {output}"
        );
        assert!(result.details["timeout"] == true);
        // Should complete in roughly 1-3 seconds, not 30
        assert!(elapsed.as_secs() < 10, "took too long: {elapsed:?}");
    }

    #[tokio::test]
    async fn missing_command_parameter() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_in(dir.path());
        let params = serde_json::json!({});
        let err = tool.execute("c6", params, cancel()).await.unwrap_err();
        match err {
            ToolError::InvalidParameters(msg) => assert!(msg.contains("command")),
            _ => panic!("expected InvalidParameters, got: {err:?}"),
        }
    }
}
