use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use rho_core::config::AgentDef;
use rho_core::tool::{AgentTool, ToolError};
use rho_core::types::{Content, ToolResult};

const DEFAULT_MAX_AGENT_DEPTH: usize = 5;

pub struct TaskTool {
    rho_binary: PathBuf,
    cwd: PathBuf,
    max_depth: usize,
    current_depth: usize,
    allowed_agents: Option<Vec<AgentDef>>,
    /// Shared reference to parent's message history for cache-optimized forking.
    context_messages: Option<Arc<tokio::sync::RwLock<Vec<rho_core::types::Message>>>>,
    cache_sharing_enabled: bool,
}

impl TaskTool {
    pub fn new(cwd: PathBuf, max_depth: Option<usize>, allowed_agents: Vec<AgentDef>) -> Self {
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

        let current_depth: usize = std::env::var("RHO_AGENT_DEPTH")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);

        let allowed_agents = if allowed_agents.is_empty() {
            None
        } else {
            Some(allowed_agents)
        };

        Self {
            rho_binary,
            cwd,
            max_depth: max_depth.unwrap_or(DEFAULT_MAX_AGENT_DEPTH),
            current_depth,
            allowed_agents,
            context_messages: None,
            cache_sharing_enabled: false,
        }
    }

    /// Enable cache sharing with a shared reference to the parent's message history.
    pub fn with_cache_sharing(
        mut self,
        messages: Arc<tokio::sync::RwLock<Vec<rho_core::types::Message>>>,
    ) -> Self {
        self.context_messages = Some(messages);
        self.cache_sharing_enabled = true;
        self
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
        let mut desc =
            "Launch a subagent to handle a task. The subagent runs as a separate process with \
         its own context. Use this for research, analysis, or delegating work that should \
         not pollute the current conversation context."
                .to_string();
        if let Some(ref agents) = self.allowed_agents {
            desc.push_str("\n\nAvailable agents:");
            for a in agents {
                desc.push_str(&format!("\n- {}", a.name));
                if let Some(ref d) = a.description {
                    desc.push_str(&format!(": {}", d));
                }
            }
        }
        desc
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let agent_prop = if let Some(ref agents) = self.allowed_agents {
            let names: Vec<String> = agents.iter().map(|a| a.name.clone()).collect();
            serde_json::json!({
                "type": "string",
                "description": "Agent to use for this task",
                "enum": names
            })
        } else {
            serde_json::json!({
                "type": "string",
                "description": "Name of agent config from .rho/agents/ (optional)"
            })
        };

        serde_json::json!({
            "type": "object",
            "required": ["prompt"],
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The task prompt for the subagent"
                },
                "agent": agent_prop,
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
        // Depth limit check
        if self.current_depth >= self.max_depth {
            return Ok(ToolResult {
                content: vec![Content::Text {
                    text: format!(
                        "Subagent depth limit reached ({}/{}). Cannot spawn further subagents.",
                        self.current_depth, self.max_depth
                    ),
                }],
                details: serde_json::json!({"error": "depth_limit"}),
            });
        }

        let prompt = params["prompt"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidParameters("prompt is required".into()))?;

        let agent_name = params["agent"].as_str();
        let tools_override = params["tools"].as_str();

        // Agent allowlist check
        let matched_agent_def = if let Some(ref allowed) = self.allowed_agents {
            if let Some(name) = agent_name {
                let found = allowed.iter().find(|a| a.name == name);
                if found.is_none() {
                    let available: Vec<&str> = allowed.iter().map(|a| a.name.as_str()).collect();
                    return Ok(ToolResult {
                        content: vec![Content::Text {
                            text: format!(
                                "Agent '{}' is not in the allowed agents list. Available agents: {}",
                                name,
                                available.join(", ")
                            ),
                        }],
                        details: serde_json::json!({"error": "agent_not_allowed"}),
                    });
                }
                found
            } else {
                None
            }
        } else {
            None
        };

        // Load agent config file if specified (file-based config)
        let agent_config = if let Some(name) = agent_name {
            load_agent_config(&self.cwd, name)
        } else {
            None
        };

        // Cache sharing: serialize parent context to temp file
        let context_file_path = if self.cache_sharing_enabled {
            if let Some(ref ctx) = self.context_messages {
                let messages = ctx.read().await;
                if !messages.is_empty() {
                    let json = serde_json::to_string(&*messages).ok();
                    if let Some(ref json_str) = json {
                        // Skip if too large (>2MB)
                        if json_str.len() <= 2 * 1024 * 1024 {
                            let path = std::env::temp_dir()
                                .join(format!("rho-context-{}.json", uuid::Uuid::new_v4()));
                            if tokio::fs::write(&path, json_str).await.is_ok() {
                                Some(path)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        let mut cmd = tokio::process::Command::new(&self.rho_binary);
        cmd.current_dir(&self.cwd);

        // Set depth env for child process
        cmd.env("RHO_AGENT_DEPTH", (self.current_depth + 1).to_string());

        // Pass context file for cache sharing
        if let Some(ref path) = context_file_path {
            cmd.arg("--context-file").arg(path);
        }

        // Apply tools restriction
        // Priority: explicit tools param > AgentDef from RHO.md > agent config file
        if let Some(tools) = tools_override {
            cmd.arg("--tools").arg(tools);
        } else if let Some(agent_def) = matched_agent_def {
            if let Some(ref tools) = agent_def.tools {
                cmd.arg("--tools").arg(tools);
            }
        } else if let Some(ref ac) = agent_config {
            if let Some(ref tools) = ac.tools {
                cmd.arg("--tools").arg(tools);
            }
        }

        // Apply model override
        // Priority: AgentDef from RHO.md > agent config file
        if let Some(agent_def) = matched_agent_def {
            if let Some(ref model) = agent_def.model {
                cmd.arg("--model").arg(model);
            }
        } else if let Some(ref ac) = agent_config {
            if let Some(ref model) = ac.model {
                cmd.arg("--model").arg(model);
            }
        }

        // Apply system prompt append (only from agent config file)
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

        let child = cmd
            .spawn()
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to spawn subagent: {}", e)))?;

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

        // Cleanup context file
        if let Some(ref path) = context_file_path {
            let _ = tokio::fs::remove_file(path).await;
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
    fn task_tool_schema_no_agents() {
        let tool = TaskTool::new(PathBuf::from("."), None, vec![]);
        let schema = tool.parameters_schema();
        assert_eq!(schema["required"][0], "prompt");
        assert!(schema["properties"]["agent"].is_object());
        assert!(schema["properties"]["agent"]["enum"].is_null());
        assert!(schema["properties"]["tools"].is_object());
    }

    #[test]
    fn task_tool_schema_with_agents() {
        let agents = vec![
            AgentDef {
                name: "researcher".into(),
                tools: Some("read,grep".into()),
                model: None,
                description: Some("Research agent".into()),
            },
            AgentDef {
                name: "coder".into(),
                tools: None,
                model: Some("claude-opus".into()),
                description: None,
            },
        ];
        let tool = TaskTool::new(PathBuf::from("."), None, agents);
        let schema = tool.parameters_schema();
        let agent_enum = &schema["properties"]["agent"]["enum"];
        assert_eq!(agent_enum[0], "researcher");
        assert_eq!(agent_enum[1], "coder");
    }

    #[test]
    fn task_tool_description_lists_agents() {
        let agents = vec![AgentDef {
            name: "researcher".into(),
            tools: None,
            model: None,
            description: Some("Finds stuff".into()),
        }];
        let tool = TaskTool::new(PathBuf::from("."), None, agents);
        let desc = tool.description();
        assert!(desc.contains("Available agents:"));
        assert!(desc.contains("- researcher: Finds stuff"));
    }

    #[tokio::test]
    async fn task_tool_depth_limit() {
        // Simulate being at max depth
        std::env::set_var("RHO_AGENT_DEPTH", "5");
        let tool = TaskTool::new(PathBuf::from("."), Some(5), vec![]);
        let cancel = CancellationToken::new();
        let params = serde_json::json!({"prompt": "do something"});
        let result = tool.execute("test", params, cancel).await.unwrap();
        let text = match &result.content[0] {
            Content::Text { text } => text.as_str(),
            _ => panic!("expected text"),
        };
        assert!(text.contains("depth limit reached"));
        assert_eq!(result.details["error"], "depth_limit");
        // Clean up
        std::env::remove_var("RHO_AGENT_DEPTH");
    }

    #[tokio::test]
    async fn task_tool_agent_not_allowed() {
        std::env::remove_var("RHO_AGENT_DEPTH");
        let agents = vec![AgentDef {
            name: "researcher".into(),
            tools: None,
            model: None,
            description: None,
        }];
        let tool = TaskTool::new(PathBuf::from("."), None, agents);
        let cancel = CancellationToken::new();
        let params = serde_json::json!({"prompt": "do something", "agent": "hacker"});
        let result = tool.execute("test", params, cancel).await.unwrap();
        let text = match &result.content[0] {
            Content::Text { text } => text.as_str(),
            _ => panic!("expected text"),
        };
        assert!(text.contains("not in the allowed agents list"));
        assert!(text.contains("researcher"));
        assert_eq!(result.details["error"], "agent_not_allowed");
    }
}
