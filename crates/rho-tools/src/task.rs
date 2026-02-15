use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use rho_core::tool::{AgentTool, ToolError};
use rho_core::types::{Content, ToolResult};

pub struct TaskTool {
    rho_binary: PathBuf,
    cwd: PathBuf,
}

impl TaskTool {
    pub fn new(cwd: PathBuf) -> Self {
        // Find the rho binary — prefer the one next to current exe
        let rho_binary = std::env::current_exe()
            .ok()
            .and_then(|p| {
                let dir = p.parent()?;
                let candidate = dir.join("rho");
                if candidate.exists() {
                    Some(candidate)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| PathBuf::from("rho"));

        Self { rho_binary, cwd }
    }
}

#[async_trait]
impl AgentTool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }

    fn label(&self) -> String {
        "Task (subagent)".into()
    }

    fn description(&self) -> String {
        "Launch a subagent to handle a task. The subagent runs as a separate process with \
         its own context. Use this for research, analysis, or delegating work that should \
         not pollute the current conversation context."
            .into()
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["prompt"],
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The task prompt for the subagent"
                },
                "agent": {
                    "type": "string",
                    "description": "Name of agent config from .rho/agents/ (optional)"
                },
                "tools": {
                    "type": "string",
                    "description": "Comma-separated list of allowed tools (e.g. 'read,grep,find')"
                }
            }
        })
    }

    async fn execute(
        &self,
        _tool_call_id: &str,
        params: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let prompt = params["prompt"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidParameters("prompt is required".into()))?;

        let agent_name = params["agent"].as_str();
        let tools_override = params["tools"].as_str();

        // Load agent config if specified
        let agent_config = if let Some(name) = agent_name {
            load_agent_config(&self.cwd, name)
        } else {
            None
        };

        let mut cmd = tokio::process::Command::new(&self.rho_binary);
        cmd.current_dir(&self.cwd);

        // Apply tools restriction
        if let Some(tools) = tools_override {
            cmd.arg("--tools").arg(tools);
        } else if let Some(ref ac) = agent_config {
            if let Some(ref tools) = ac.tools {
                cmd.arg("--tools").arg(tools);
            }
        }

        // Apply model override
        if let Some(ref ac) = agent_config {
            if let Some(ref model) = ac.model {
                cmd.arg("--model").arg(model);
            }
        }

        // Apply system prompt append
        if let Some(ref ac) = agent_config {
            if let Some(ref append) = ac.system_prompt_append {
                cmd.arg("--system-append").arg(append);
            }
        }

        // The prompt is the positional argument
        cmd.arg(prompt);

        // Capture output
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let child = cmd.spawn().map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to spawn subagent: {}", e))
        })?;

        let output: std::process::Output = tokio::select! {
            result = child.wait_with_output() => {
                result.map_err(|e| ToolError::ExecutionFailed(format!("Subagent error: {}", e)))?
            }
            _ = cancel.cancelled() => {
                return Ok(ToolResult {
                    content: vec![Content::Text {
                        text: "Subagent cancelled".into(),
                    }],
                    details: serde_json::json!({}),
                });
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let mut result_text = stdout.to_string();
        if !stderr.is_empty() && !output.status.success() {
            result_text.push_str("\n\n[stderr]\n");
            result_text.push_str(&stderr);
        }

        // Truncate to 20KB
        if result_text.len() > 20_000 {
            result_text.truncate(20_000);
            result_text.push_str("\n... [truncated]");
        }

        Ok(ToolResult {
            content: vec![Content::Text { text: result_text }],
            details: serde_json::json!({
                "exit_code": output.status.code(),
            }),
        })
    }
}

#[derive(Debug, Clone)]
struct AgentConfig {
    tools: Option<String>,
    model: Option<String>,
    system_prompt_append: Option<String>,
}

/// Load agent config from .rho/agents/{name}.md or .claude/agents/{name}.md
fn load_agent_config(cwd: &Path, name: &str) -> Option<AgentConfig> {
    let candidates = [
        cwd.join(format!(".rho/agents/{}.md", name)),
        cwd.join(format!(".claude/agents/{}.md", name)),
    ];

    for path in &candidates {
        if let Ok(content) = std::fs::read_to_string(path) {
            return Some(parse_agent_config(&content));
        }
    }

    // Check home directory
    if let Some(home) = dirs::home_dir() {
        let path = home.join(format!(".rho/agents/{}.md", name));
        if let Ok(content) = std::fs::read_to_string(&path) {
            return Some(parse_agent_config(&content));
        }
    }

    None
}

fn parse_agent_config(content: &str) -> AgentConfig {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return AgentConfig {
            tools: None,
            model: None,
            system_prompt_append: Some(content.to_string()),
        };
    }

    let after_first = &trimmed[3..];
    let Some(end) = after_first.find("\n---") else {
        return AgentConfig {
            tools: None,
            model: None,
            system_prompt_append: Some(content.to_string()),
        };
    };

    let frontmatter = &after_first[..end];
    let body_start = 3 + end + 4;
    let body = trimmed[body_start..].trim().to_string();

    let mut config = AgentConfig {
        tools: None,
        model: None,
        system_prompt_append: if body.is_empty() { None } else { Some(body) },
    };

    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("tools:") {
            config.tools = Some(val.trim().to_string());
        } else if let Some(val) = line.strip_prefix("model:") {
            config.model = Some(val.trim().to_string());
        }
    }

    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_agent_config_with_frontmatter() {
        let content = "\
---
name: researcher
tools: read,grep,find
model: claude-sonnet-4-5-20250929
---
You are a research agent. Analyze code and return findings.
Do not modify any files.";

        let config = parse_agent_config(content);
        assert_eq!(config.tools.as_deref(), Some("read,grep,find"));
        assert_eq!(config.model.as_deref(), Some("claude-sonnet-4-5-20250929"));
        assert!(config
            .system_prompt_append
            .unwrap()
            .contains("research agent"));
    }

    #[test]
    fn parse_agent_config_no_frontmatter() {
        let content = "Just do research.";
        let config = parse_agent_config(content);
        assert!(config.tools.is_none());
        assert!(config.model.is_none());
        assert_eq!(config.system_prompt_append.as_deref(), Some(content));
    }

    #[test]
    fn task_tool_schema() {
        let tool = TaskTool::new(PathBuf::from("."));
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"][0], "prompt");
        assert!(schema["properties"]["agent"].is_object());
        assert!(schema["properties"]["tools"].is_object());
    }
}
