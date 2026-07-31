// Bash tool — PTY command execution with timeout and output truncation

use async_trait::async_trait;
use portable_pty::{CommandBuilder, PtySize};
use rho_core::tool::{AgentTool, ToolError};
use rho_core::types::{Content, ToolResult};
use serde_json::Value;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const DEFAULT_TIMEOUT_SECS: u64 = 300;
const MAX_TIMEOUT_SECS: u64 = 3600;
const MAX_OUTPUT_BYTES: usize = 102_400; // 100KB
const TRUNCATION_EDGE: usize = 10_240; // 10KB kept at each end
const TERMINATION_GRACE: Duration = Duration::from_millis(500);

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

/// Terminate the command and, on Unix, its process group. `portable-pty`
/// starts the child as a session leader, so its pid is also its process-group
/// id. Killing the group prevents a shell's children from surviving a timeout
/// or cancellation. Other platforms get portable-pty's best-effort child kill.
fn terminate_command(
    pid: Option<u32>,
    killer: &mut (dyn portable_pty::ChildKiller + Send + Sync),
    force: bool,
) {
    #[cfg(unix)]
    if let Some(pid) = pid.and_then(|pid| i32::try_from(pid).ok()) {
        const SIGHUP: i32 = 1;
        const SIGKILL: i32 = 9;
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        // Negative pid addresses the process group created by portable-pty.
        // Ignore ESRCH: the command may have exited between select and here.
        let _ = unsafe { kill(-pid, if force { SIGKILL } else { SIGHUP }) };
        return;
    }

    if !force {
        let _ = killer.kill();
    }
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
        cancel: CancellationToken,
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

        let pid = child.process_id();
        let mut killer = child.clone_killer();

        // Stream output out of the blocking reader so timeout results can keep
        // everything received before termination.
        let (output_tx, mut output_rx) = tokio::sync::mpsc::unbounded_channel();
        let _read_handle = tokio::task::spawn_blocking(move || {
            let mut chunk = [0_u8; 8192];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(read) if output_tx.send(chunk[..read].to_vec()).is_err() => break,
                    Ok(_) => {}
                }
            }
        });

        // Wait for the child in a blocking thread
        let mut wait_handle = tokio::task::spawn_blocking(move || child.wait());
        let deadline = tokio::time::sleep(Duration::from_secs(timeout_secs));
        tokio::pin!(deadline);
        let mut output_bytes = Vec::new();

        enum End {
            Exited(Result<portable_pty::ExitStatus, std::io::Error>),
            Timeout,
            Cancelled,
        }

        let end = loop {
            tokio::select! {
                status = &mut wait_handle => {
                    break End::Exited(status.map_err(|e| {
                        ToolError::ExecutionFailed(format!("wait task panicked: {e}"))
                    })?);
                }
                chunk = output_rx.recv() => {
                    if let Some(chunk) = chunk {
                        output_bytes.extend_from_slice(&chunk);
                    }
                }
                _ = &mut deadline => break End::Timeout,
                _ = cancel.cancelled() => break End::Cancelled,
            }
        };

        match end {
            End::Exited(exit_status) => {
                let exit_status = exit_status.map_err(|e| {
                    ToolError::ExecutionFailed(format!("failed to wait on child: {e}"))
                })?;
                if tokio::time::timeout(TERMINATION_GRACE, async {
                    while let Some(chunk) = output_rx.recv().await {
                        output_bytes.extend_from_slice(&chunk);
                    }
                })
                .await
                .is_err()
                {
                    // A detached descendant still owns the PTY. Do not let it
                    // keep this tool invocation open after the shell exited.
                    terminate_command(pid, killer.as_mut(), true);
                }
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
            End::Timeout => {
                terminate_command(pid, killer.as_mut(), false);
                let drain_deadline = tokio::time::sleep(TERMINATION_GRACE);
                tokio::pin!(drain_deadline);
                loop {
                    tokio::select! {
                        chunk = output_rx.recv() => match chunk {
                            Some(chunk) => output_bytes.extend_from_slice(&chunk),
                            None => break,
                        },
                        _ = &mut drain_deadline => {
                            terminate_command(pid, killer.as_mut(), true);
                            break;
                        }
                    }
                }
                let partial = truncate_output(&String::from_utf8_lossy(&output_bytes));
                let partial = if partial.is_empty() {
                    "(no output before timeout)".to_string()
                } else {
                    partial
                };

                Ok(ToolResult {
                    content: vec![Content::Text {
                        text: format!("Command timed out after {timeout_secs} seconds.\n{partial}"),
                    }],
                    details: serde_json::json!({"timeout": true}),
                })
            }
            End::Cancelled => {
                terminate_command(pid, killer.as_mut(), false);
                let termination_deadline = tokio::time::sleep(TERMINATION_GRACE);
                tokio::pin!(termination_deadline);
                loop {
                    tokio::select! {
                        chunk = output_rx.recv() => match chunk {
                            Some(_) => {}
                            None => break,
                        },
                        _ = &mut termination_deadline => {
                            terminate_command(pid, killer.as_mut(), true);
                            break;
                        }
                    }
                }
                Err(ToolError::Cancelled)
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
        let params =
            serde_json::json!({"command": "printf before-timeout; sleep 30", "timeout": 1});
        let start = Instant::now();
        let result = tool.execute("c5", params, cancel()).await.unwrap();
        let elapsed = start.elapsed();
        let output = text_of(&result);
        assert!(
            output.contains("timed out"),
            "expected timeout message: {output}"
        );
        assert!(result.details["timeout"] == true);
        assert!(
            output.contains("before-timeout"),
            "partial output lost: {output}"
        );
        // Should complete in roughly 1-3 seconds, not 30
        assert!(elapsed.as_secs() < 10, "took too long: {elapsed:?}");
    }

    #[tokio::test]
    async fn cancellation_stops_running_command_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let started = dir.path().join("started");
        let escaped = dir.path().join("escaped");
        let tool = tool_in(dir.path());
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            let params = serde_json::json!({
                "command": "touch started; (sleep 1; touch escaped) & wait",
                "timeout": 30
            });
            tool.execute("cancel", params, task_cancel).await
        });

        tokio::time::timeout(Duration::from_secs(2), async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("command did not start");

        let start = Instant::now();
        cancel.cancel();
        let err = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("cancellation did not finish promptly")
            .expect("tool task panicked")
            .unwrap_err();
        assert!(matches!(err, ToolError::Cancelled));
        assert!(start.elapsed() < Duration::from_secs(2));

        // The background grandchild belongs to the PTY process group and must
        // not outlive the cancelled shell.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(!escaped.exists(), "a descendant survived cancellation");
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
