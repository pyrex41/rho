use std::time::Duration;
use async_trait::async_trait;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

// === Hook Events ===

/// All lifecycle events that hooks can respond to.
#[derive(Debug, Clone, PartialEq)]
pub enum HookEvent {
    /// Before a tool executes. Can veto or modify input.
    PreToolUse,
    /// After all tools in a batch complete (existing behavior).
    PostToolUse,
    /// When the agent loop starts.
    SessionStart,
    /// At the end of each LLM turn (after post-tools hooks).
    TurnEnd,
}

impl Default for HookEvent {
    fn default() -> Self {
        Self::PostToolUse
    }
}

// === Structured Hook Output ===

/// Structured JSON output from a hook process.
/// Falls back to plain text if stdout isn't valid JSON (backwards compatible).
#[derive(Debug, Clone, Deserialize)]
pub struct HookOutput {
    #[serde(default = "default_true", rename = "continue")]
    pub continue_execution: bool,
    #[serde(default)]
    pub decision: Option<HookDecision>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub modified_input: Option<serde_json::Value>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum HookDecision {
    Approve,
    Deny,
}

/// Parse hook stdout as structured JSON, falling back to plain text.
pub fn parse_hook_output(stdout: &str) -> HookOutput {
    serde_json::from_str(stdout).unwrap_or_else(|_| HookOutput {
        continue_execution: true,
        decision: None,
        message: if stdout.trim().is_empty() {
            None
        } else {
            Some(stdout.to_string())
        },
        modified_input: None,
    })
}

// === Tool Matcher ===

/// Matches tool names with optional argument patterns.
/// Syntax: `"bash"` (matches all bash calls) or `"bash(git *)"` (matches bash with git prefix).
#[derive(Debug, Clone)]
pub struct ToolMatcher {
    pub tool_name: String,
    pub arg_pattern: Option<String>,
}

impl ToolMatcher {
    pub fn parse(spec: &str) -> Self {
        let spec = spec.trim();
        if let Some(paren_start) = spec.find('(') {
            if spec.ends_with(')') {
                let tool_name = spec[..paren_start].trim().to_string();
                let arg_pattern = spec[paren_start + 1..spec.len() - 1].trim().to_string();
                return Self {
                    tool_name,
                    arg_pattern: if arg_pattern.is_empty() {
                        None
                    } else {
                        Some(arg_pattern)
                    },
                };
            }
        }
        Self {
            tool_name: spec.to_string(),
            arg_pattern: None,
        }
    }

    /// Check if this matcher matches the given tool name and arguments.
    /// The pattern is matched against the JSON-serialized args using substring glob matching.
    pub fn matches(&self, tool_name: &str, args: &serde_json::Value) -> bool {
        if self.tool_name != tool_name {
            return false;
        }
        match &self.arg_pattern {
            None => true,
            Some(pattern) => {
                let args_str = args.to_string();
                // Check if pattern matches anywhere in the stringified args
                glob_contains(pattern, &args_str)
            }
        }
    }
}

/// Check if the glob pattern matches anywhere within the text.
fn glob_contains(pattern: &str, text: &str) -> bool {
    // For patterns without *, check simple substring containment
    if !pattern.contains('*') {
        return text.contains(pattern);
    }
    // Split pattern on *, check that all parts appear in order within text
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut pos = 0;
    for part in &parts {
        if part.is_empty() {
            continue;
        }
        if let Some(found) = text[pos..].find(part) {
            pos += found + part.len();
        } else {
            return false;
        }
    }
    true
}

/// Simple glob matching: `*` matches any sequence of characters.
#[cfg(test)]
fn glob_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return text == pattern;
    }
    let mut pos = 0;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if let Some(found) = text[pos..].find(part) {
            if i == 0 && found != 0 {
                return false; // Must match from start if pattern doesn't start with *
            }
            pos += found + part.len();
        } else {
            return false;
        }
    }
    // If pattern doesn't end with *, text must end exactly
    if !pattern.ends_with('*') {
        return text.ends_with(parts.last().unwrap_or(&""));
    }
    true
}

// === Pre-Tool-Use Hook ===

/// Result of a pre-tool-use hook.
#[derive(Debug, Clone)]
pub enum PreToolUseResult {
    /// Allow the tool to execute normally.
    Allow,
    /// Allow but with modified input arguments.
    AllowWithModifiedInput(serde_json::Value),
    /// Deny the tool execution with a reason.
    Deny { reason: String },
}

/// Hook that runs before a tool executes.
#[async_trait]
pub trait PreToolUseHook: Send + Sync {
    fn name(&self) -> &str;
    /// Execute the hook. Returns whether to allow/deny/modify the tool call.
    async fn execute(
        &self,
        tool_name: &str,
        tool_args: &serde_json::Value,
        cancel: CancellationToken,
    ) -> PreToolUseResult;
    fn timeout(&self) -> Duration {
        Duration::from_secs(30)
    }
}

// === Lifecycle Hook ===

/// Result of a lifecycle hook execution.
#[derive(Debug, Clone)]
pub struct LifecycleHookResult {
    pub steering_message: Option<String>,
    pub success: bool,
    pub summary: String,
}

/// Hook for lifecycle events (SessionStart, TurnEnd).
#[async_trait]
pub trait LifecycleHook: Send + Sync {
    fn name(&self) -> &str;
    fn event(&self) -> HookEvent;
    async fn execute(&self, cancel: CancellationToken) -> LifecycleHookResult;
    fn timeout(&self) -> Duration {
        Duration::from_secs(30)
    }
}

// === Post-Tools Hook (existing) ===

/// Result of a post-tools hook execution.
#[derive(Debug, Clone)]
pub struct PostToolsHookResult {
    pub steering_message: Option<String>,
    pub success: bool,
    pub summary: String,
}

/// Hook that runs after tool execution, before returning to LLM.
#[async_trait]
pub trait PostToolsHook: Send + Sync {
    fn name(&self) -> &str;
    async fn execute(
        &self,
        tool_names_called: &[String],
        cancel: CancellationToken,
    ) -> PostToolsHookResult;
    fn timeout(&self) -> Duration {
        Duration::from_secs(30)
    }
}

// === Hook Configuration ===

/// Configuration for a hook loaded from RHO.md.
#[derive(Debug, Clone)]
pub struct HookConfig {
    pub name: String,
    pub command: String,
    pub timeout: u64,
    pub inject_on_failure: bool,
    pub trigger_tools: Option<Vec<String>>,
    /// Which event this hook responds to. Default: PostToolUse.
    pub event: HookEvent,
    /// Advanced tool matchers (e.g., "bash(git *)").
    pub tool_matchers: Option<Vec<ToolMatcher>>,
}

impl Default for HookConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            command: String::new(),
            timeout: 30,
            inject_on_failure: true,
            trigger_tools: None,
            event: HookEvent::default(),
            tool_matchers: None,
        }
    }
}

// === Shell Command Implementations ===

/// Core shell execution logic shared across hook types.
async fn run_shell_hook(
    name: &str,
    command: &str,
    cwd: &std::path::Path,
    env_vars: &[(&str, &str)],
    inject_on_failure: bool,
    cancel: CancellationToken,
) -> (bool, String, Option<String>) {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(command).current_dir(cwd);
    for (k, v) in env_vars {
        cmd.env(k, v);
    }

    let result = tokio::select! {
        result = cmd.output() => result,
        _ = cancel.cancelled() => {
            return (false, format!("{}: cancelled", name), None);
        }
    };

    match result {
        Ok(output) => {
            let success = output.status.success();
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            let steering = if !success && inject_on_failure {
                let mut msg = format!(
                    "[hook:{}] FAILED (exit {})\n",
                    name,
                    output.status.code().unwrap_or(-1)
                );
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
                format!("{}: ok", name)
            } else {
                format!(
                    "{}: failed (exit {})",
                    name,
                    output.status.code().unwrap_or(-1)
                )
            };

            (success, summary, steering)
        }
        Err(e) => {
            let steering = if inject_on_failure {
                Some(format!("[hook:{}] Failed to execute: {}", name, e))
            } else {
                None
            };
            (false, format!("{}: error ({})", name, e), steering)
        }
    }
}

/// Shell command hook for post-tools events (backwards compatible).
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

    async fn execute(
        &self,
        tool_names_called: &[String],
        cancel: CancellationToken,
    ) -> PostToolsHookResult {
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

        let (success, summary, steering) = run_shell_hook(
            &self.config.name,
            &self.config.command,
            &self.cwd,
            &[],
            self.config.inject_on_failure,
            cancel,
        )
        .await;

        PostToolsHookResult {
            steering_message: steering,
            success,
            summary,
        }
    }
}

/// Shell command hook for pre-tool-use events.
pub struct ShellCommandPreToolHook {
    pub config: HookConfig,
    pub cwd: std::path::PathBuf,
}

#[async_trait]
impl PreToolUseHook for ShellCommandPreToolHook {
    fn name(&self) -> &str {
        &self.config.name
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(self.config.timeout)
    }

    async fn execute(
        &self,
        tool_name: &str,
        tool_args: &serde_json::Value,
        cancel: CancellationToken,
    ) -> PreToolUseResult {
        // Check tool_matchers first, then trigger_tools
        if let Some(ref matchers) = self.config.tool_matchers {
            let any_match = matchers.iter().any(|m| m.matches(tool_name, tool_args));
            if !any_match {
                return PreToolUseResult::Allow;
            }
        } else if let Some(ref triggers) = self.config.trigger_tools {
            if !triggers.iter().any(|t| t == tool_name) {
                return PreToolUseResult::Allow;
            }
        }

        let args_str = tool_args.to_string();

        let result = tokio::select! {
            result = tokio::process::Command::new("sh")
                .arg("-c")
                .arg(&self.config.command)
                .current_dir(&self.cwd)
                .env("RHO_HOOK_EVENT", "pre_tool_use")
                .env("RHO_TOOL_NAME", tool_name)
                .env("RHO_TOOL_ARGS", &args_str)
                .output() => result,
            _ = cancel.cancelled() => {
                return PreToolUseResult::Allow; // Don't block on cancel
            }
        };

        match result {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let hook_output = parse_hook_output(&stdout);

                if let Some(HookDecision::Deny) = hook_output.decision {
                    return PreToolUseResult::Deny {
                        reason: hook_output
                            .message
                            .unwrap_or_else(|| "Hook denied execution".into()),
                    };
                }

                if !hook_output.continue_execution {
                    return PreToolUseResult::Deny {
                        reason: hook_output
                            .message
                            .unwrap_or_else(|| "Hook stopped execution".into()),
                    };
                }

                if let Some(modified) = hook_output.modified_input {
                    return PreToolUseResult::AllowWithModifiedInput(modified);
                }

                // Non-zero exit without structured output = deny
                if !output.status.success() && self.config.inject_on_failure {
                    return PreToolUseResult::Deny {
                        reason: format!(
                            "[hook:{}] exit {} — {}",
                            self.config.name,
                            output.status.code().unwrap_or(-1),
                            stdout.trim()
                        ),
                    };
                }

                PreToolUseResult::Allow
            }
            Err(_) => PreToolUseResult::Allow, // Don't block on hook failure
        }
    }
}

/// Shell command hook for lifecycle events (SessionStart, TurnEnd).
pub struct ShellCommandLifecycleHook {
    pub config: HookConfig,
    pub cwd: std::path::PathBuf,
}

#[async_trait]
impl LifecycleHook for ShellCommandLifecycleHook {
    fn name(&self) -> &str {
        &self.config.name
    }

    fn event(&self) -> HookEvent {
        self.config.event.clone()
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(self.config.timeout)
    }

    async fn execute(&self, cancel: CancellationToken) -> LifecycleHookResult {
        let event_str = match self.config.event {
            HookEvent::SessionStart => "session_start",
            HookEvent::TurnEnd => "turn_end",
            _ => "unknown",
        };

        let (success, summary, steering) = run_shell_hook(
            &self.config.name,
            &self.config.command,
            &self.cwd,
            &[("RHO_HOOK_EVENT", event_str)],
            self.config.inject_on_failure,
            cancel,
        )
        .await;

        LifecycleHookResult {
            steering_message: steering,
            success,
            summary,
        }
    }
}

// === Parsing helper for HookEvent ===

pub fn parse_hook_event(s: &str) -> HookEvent {
    match s.trim().to_lowercase().as_str() {
        "pre_tool_use" | "pretooluse" => HookEvent::PreToolUse,
        "post_tool_use" | "posttooluse" => HookEvent::PostToolUse,
        "session_start" | "sessionstart" => HookEvent::SessionStart,
        "turn_end" | "turnend" => HookEvent::TurnEnd,
        _ => HookEvent::PostToolUse,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_matcher_simple() {
        let m = ToolMatcher::parse("bash");
        assert_eq!(m.tool_name, "bash");
        assert!(m.arg_pattern.is_none());
        assert!(m.matches("bash", &serde_json::json!({})));
        assert!(!m.matches("read", &serde_json::json!({})));
    }

    #[test]
    fn test_tool_matcher_with_pattern() {
        let m = ToolMatcher::parse("bash(git *)");
        assert_eq!(m.tool_name, "bash");
        assert_eq!(m.arg_pattern.as_deref(), Some("git *"));
        // The args are serialized to JSON, so "git" must appear in the stringified args
        assert!(m.matches("bash", &serde_json::json!({"command": "git status"})));
        assert!(!m.matches("bash", &serde_json::json!({"command": "cargo test"})));
    }

    #[test]
    fn test_tool_matcher_parse_edge_cases() {
        let m = ToolMatcher::parse("  write  ");
        assert_eq!(m.tool_name, "write");

        let m = ToolMatcher::parse("bash()");
        assert_eq!(m.tool_name, "bash");
        assert!(m.arg_pattern.is_none()); // empty parens = no pattern
    }

    #[test]
    fn test_glob_match() {
        assert!(glob_match("git *", "git status"));
        assert!(glob_match("git *", "git commit -m foo"));
        assert!(!glob_match("git *", "cargo test"));
        assert!(glob_match("*test*", "cargo test --quiet"));
        assert!(!glob_match("*test*", "cargo build"));
        assert!(glob_match("exact", "exact"));
        assert!(!glob_match("exact", "not exact"));
    }

    #[test]
    fn test_parse_hook_output_json() {
        let json = r#"{"continue": false, "decision": "deny", "message": "Blocked"}"#;
        let output = parse_hook_output(json);
        assert!(!output.continue_execution);
        assert_eq!(output.decision, Some(HookDecision::Deny));
        assert_eq!(output.message.as_deref(), Some("Blocked"));
    }

    #[test]
    fn test_parse_hook_output_plain_text() {
        let output = parse_hook_output("some error output");
        assert!(output.continue_execution);
        assert!(output.decision.is_none());
        assert_eq!(output.message.as_deref(), Some("some error output"));
    }

    #[test]
    fn test_parse_hook_output_empty() {
        let output = parse_hook_output("");
        assert!(output.continue_execution);
        assert!(output.message.is_none());
    }

    #[test]
    fn test_parse_hook_event() {
        assert_eq!(parse_hook_event("pre_tool_use"), HookEvent::PreToolUse);
        assert_eq!(parse_hook_event("session_start"), HookEvent::SessionStart);
        assert_eq!(parse_hook_event("turn_end"), HookEvent::TurnEnd);
        assert_eq!(parse_hook_event("post_tool_use"), HookEvent::PostToolUse);
        assert_eq!(parse_hook_event("unknown"), HookEvent::PostToolUse);
    }

    #[test]
    fn test_hook_config_default() {
        let config = HookConfig::default();
        assert_eq!(config.event, HookEvent::PostToolUse);
        assert_eq!(config.timeout, 30);
        assert!(config.inject_on_failure);
    }
}
