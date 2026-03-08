use std::time::Duration;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

/// Result of a post-tools hook execution
#[derive(Debug, Clone)]
pub struct PostToolsHookResult {
    /// If set, injected as a User message before next LLM call
    pub steering_message: Option<String>,
    pub success: bool,
    pub summary: String,
}

/// Hook that runs after tool execution, before returning to LLM
#[async_trait]
pub trait PostToolsHook: Send + Sync {
    fn name(&self) -> &str;
    async fn execute(&self, tool_names_called: &[String], cancel: CancellationToken) -> PostToolsHookResult;
    fn timeout(&self) -> Duration {
        Duration::from_secs(30)
    }
}

/// Configuration for a shell-command hook loaded from RHO.md
#[derive(Debug, Clone)]
pub struct HookConfig {
    pub name: String,
    pub command: String,
    pub timeout: u64,
    pub inject_on_failure: bool,
    pub trigger_tools: Option<Vec<String>>,
}

/// Shell command hook implementation
pub struct ShellCommandHook {
    pub config: HookConfig,
    pub cwd: std::path::PathBuf,
}

#[async_trait]
impl PostToolsHook for ShellCommandHook {
    fn name(&self) -> &str {
        &self.config.name
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(self.config.timeout)
    }

    async fn execute(&self, tool_names_called: &[String], cancel: CancellationToken) -> PostToolsHookResult {
        // Check trigger_tools filter
        if let Some(ref triggers) = self.config.trigger_tools {
            let any_match = tool_names_called.iter().any(|t| triggers.contains(t));
            if !any_match {
                return PostToolsHookResult {
                    steering_message: None,
                    success: true,
                    summary: format!("{}: skipped (no matching tools)", self.config.name),
                };
            }
        }

        let result = tokio::select! {
            result = tokio::process::Command::new("sh")
                .arg("-c")
                .arg(&self.config.command)
                .current_dir(&self.cwd)
                .output() => result,
            _ = cancel.cancelled() => {
                return PostToolsHookResult {
                    steering_message: None,
                    success: false,
                    summary: format!("{}: cancelled", self.config.name),
                };
            }
        };

        match result {
            Ok(output) => {
                let success = output.status.success();
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                let steering = if !success && self.config.inject_on_failure {
                    let mut msg = format!("[hook:{}] FAILED (exit {})\n", self.config.name, output.status.code().unwrap_or(-1));
                    if !stdout.is_empty() {
                        msg.push_str(&format!("stdout:\n{}\n", stdout.trim()));
                    }
                    if !stderr.is_empty() {
                        msg.push_str(&format!("stderr:\n{}\n", stderr.trim()));
                    }
                    Some(msg)
                } else {
                    None
                };

                let summary = if success {
                    format!("{}: ok", self.config.name)
                } else {
                    format!("{}: failed (exit {})", self.config.name, output.status.code().unwrap_or(-1))
                };

                PostToolsHookResult {
                    steering_message: steering,
                    success,
                    summary,
                }
            }
            Err(e) => PostToolsHookResult {
                steering_message: if self.config.inject_on_failure {
                    Some(format!("[hook:{}] Failed to execute: {}", self.config.name, e))
                } else {
                    None
                },
                success: false,
                summary: format!("{}: error ({})", self.config.name, e),
            },
        }
    }
}
