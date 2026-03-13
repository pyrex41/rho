use std::path::{Path, PathBuf};

use crate::hooks::HookConfig;
use crate::types::ThinkingLevel;

#[derive(Debug, Clone)]
pub struct AgentDef {
    pub name: String,
    pub tools: Option<String>,
    pub model: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProjectConfig {
    pub model: Option<String>,
    pub thinking: Option<ThinkingLevel>,
    pub system_prompt: Option<String>,
    pub system_prompt_append: Option<String>,
    pub allowed_tools: Option<Vec<String>>,
    pub validation_commands: Vec<String>,
    pub compact_threshold: Option<f64>,
    pub memories: bool,
    pub source: Option<PathBuf>,
    pub post_tools_hooks: Vec<HookConfig>,
    pub agents: Vec<AgentDef>,
    pub max_agent_depth: Option<usize>,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            model: None,
            thinking: None,
            system_prompt: None,
            system_prompt_append: None,
            allowed_tools: None,
            validation_commands: Vec::new(),
            compact_threshold: None,
            memories: true,
            source: None,
            post_tools_hooks: Vec::new(),
            agents: Vec::new(),
            max_agent_depth: None,
        }
    }
}

/// Load project configuration from RHO.md or CLAUDE.md.
///
/// Discovery order: RHO.md in cwd -> CLAUDE.md in cwd -> ~/.rho/RHO.md
pub fn load_project_config(cwd: &Path) -> ProjectConfig {
    // Try RHO.md first
    let rho_md = cwd.join("RHO.md");
    if rho_md.is_file() {
        if let Ok(content) = std::fs::read_to_string(&rho_md) {
            return parse_rho_md(&content, rho_md);
        }
    }

    // Fallback to CLAUDE.md
    let claude_md = cwd.join("CLAUDE.md");
    if claude_md.is_file() {
        if let Ok(content) = std::fs::read_to_string(&claude_md) {
            return ProjectConfig {
                system_prompt_append: Some(content),
                source: Some(claude_md),
                ..Default::default()
            };
        }
    }

    // Global fallback
    if let Some(home) = dirs::home_dir() {
        let global_rho = home.join(".rho").join("RHO.md");
        if global_rho.is_file() {
            if let Ok(content) = std::fs::read_to_string(&global_rho) {
                return parse_rho_md(&content, global_rho);
            }
        }
    }

    ProjectConfig::default()
}

/// Parse RHO.md with YAML frontmatter + markdown body.
fn parse_rho_md(content: &str, path: PathBuf) -> ProjectConfig {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        // No frontmatter — treat entire file as system_prompt_append
        return ProjectConfig {
            system_prompt_append: Some(content.to_string()),
            source: Some(path),
            ..Default::default()
        };
    }

    let after_first = &trimmed[3..];
    let Some(end) = after_first.find("\n---") else {
        return ProjectConfig {
            system_prompt_append: Some(content.to_string()),
            source: Some(path),
            ..Default::default()
        };
    };

    let frontmatter = &after_first[..end];
    let body_start = 3 + end + 4; // "---" + frontmatter + "\n---"
    let body = trimmed[body_start..].trim_start_matches('\n');

    let mut config = ProjectConfig {
        source: Some(path),
        ..Default::default()
    };

    if !body.is_empty() {
        config.system_prompt_append = Some(body.to_string());
    }

    // Parse simple YAML frontmatter (key: value lines)
    let mut in_list: Option<String> = None;
    let mut list_items: Vec<String> = Vec::new();
    // State for parsing post_tools_hooks (nested YAML objects in a list)
    let mut in_hooks = false;
    let mut current_hook: Option<HookConfig> = None;
    let mut hooks: Vec<HookConfig> = Vec::new();
    // State for parsing agents (nested YAML objects in a list)
    let mut in_agents = false;
    let mut current_agent: Option<AgentDef> = None;
    let mut agents: Vec<AgentDef> = Vec::new();

    for line in frontmatter.lines() {
        let trimmed_line = line.trim();

        // Handle agents parsing
        if in_agents {
            if trimmed_line.starts_with("- ") {
                // New agent entry — finalize previous one
                if let Some(agent) = current_agent.take() {
                    agents.push(agent);
                }
                let rest = trimmed_line.strip_prefix("- ").unwrap().trim();
                let mut agent = AgentDef {
                    name: String::new(),
                    tools: None,
                    model: None,
                    description: None,
                };
                if let Some((k, v)) = rest.split_once(':') {
                    let k = k.trim();
                    let v = v.trim();
                    apply_agent_field(&mut agent, k, v);
                }
                current_agent = Some(agent);
                continue;
            } else if trimmed_line.is_empty() {
                continue;
            } else if line.starts_with("    ") || line.starts_with("\t\t") || line.starts_with("  ") {
                // Indented line within an agent block
                if let Some(ref mut agent) = current_agent {
                    if let Some((k, v)) = trimmed_line.split_once(':') {
                        let k = k.trim();
                        let v = v.trim();
                        apply_agent_field(agent, k, v);
                    }
                }
                continue;
            } else {
                // End of agents section
                if let Some(agent) = current_agent.take() {
                    agents.push(agent);
                }
                config.agents = std::mem::take(&mut agents);
                in_agents = false;
                // Fall through to normal parsing for this line
            }
        }

        // Handle post_tools_hooks parsing
        if in_hooks {
            if trimmed_line.starts_with("- ") {
                // New hook entry — finalize previous one
                if let Some(hook) = current_hook.take() {
                    hooks.push(hook);
                }
                // Parse "- name: value" or just "- name:"
                let rest = trimmed_line.strip_prefix("- ").unwrap().trim();
                let mut hook = HookConfig {
                    name: String::new(),
                    command: String::new(),
                    timeout: 30,
                    inject_on_failure: true,
                    trigger_tools: None,
                };
                if let Some((k, v)) = rest.split_once(':') {
                    let k = k.trim();
                    let v = v.trim();
                    apply_hook_field(&mut hook, k, v);
                }
                current_hook = Some(hook);
                continue;
            } else if trimmed_line.is_empty() {
                continue;
            } else if line.starts_with("    ") || line.starts_with("\t\t") || line.starts_with("  ") {
                // Indented line within a hook block
                if let Some(ref mut hook) = current_hook {
                    if let Some((k, v)) = trimmed_line.split_once(':') {
                        let k = k.trim();
                        let v = v.trim();
                        apply_hook_field(hook, k, v);
                    }
                }
                continue;
            } else {
                // End of hooks section — non-indented, non-list line
                if let Some(hook) = current_hook.take() {
                    hooks.push(hook);
                }
                config.post_tools_hooks = std::mem::take(&mut hooks);
                in_hooks = false;
                // Fall through to normal parsing for this line
            }
        }

        // Check if this is a list item for current key
        if let Some(ref key) = in_list {
            if let Some(item) = trimmed_line.strip_prefix("- ") {
                list_items.push(item.trim().to_string());
                continue;
            } else {
                // End of list — apply accumulated items
                apply_list_field(&mut config, key, &list_items);
                list_items.clear();
                in_list = None;
            }
        }

        if let Some((key, value)) = trimmed_line.split_once(':') {
            let key = key.trim();
            let value = value.trim();

            if value.is_empty() {
                if key == "post_tools_hooks" {
                    in_hooks = true;
                    continue;
                }
                if key == "agents" {
                    in_agents = true;
                    continue;
                }
                // Could be start of a list
                in_list = Some(key.to_string());
                continue;
            }

            match key {
                "model" => config.model = Some(value.to_string()),
                "thinking" => config.thinking = Some(parse_thinking(value)),
                "compact_threshold" => {
                    if let Ok(v) = value.parse::<f64>() {
                        config.compact_threshold = Some(v);
                    }
                }
                "memories" => {
                    config.memories = value != "false";
                }
                "max_agent_depth" => {
                    if let Ok(v) = value.parse::<usize>() {
                        config.max_agent_depth = Some(v);
                    }
                }
                "allowed_tools" => {
                    // Inline comma-separated list
                    config.allowed_tools = Some(
                        value.split(',').map(|s| s.trim().to_string()).collect(),
                    );
                }
                _ => {}
            }
        }
    }

    // Flush any trailing list
    if let Some(ref key) = in_list {
        apply_list_field(&mut config, key, &list_items);
    }

    // Flush any trailing hooks
    if in_hooks {
        if let Some(hook) = current_hook.take() {
            hooks.push(hook);
        }
        config.post_tools_hooks = hooks;
    }

    // Flush any trailing agents
    if in_agents {
        if let Some(agent) = current_agent.take() {
            agents.push(agent);
        }
        config.agents = agents;
    }

    config
}

fn apply_agent_field(agent: &mut AgentDef, key: &str, value: &str) {
    match key {
        "name" => agent.name = value.to_string(),
        "tools" => agent.tools = Some(value.to_string()),
        "model" => agent.model = Some(value.to_string()),
        "description" => agent.description = Some(value.to_string()),
        _ => {}
    }
}

fn apply_hook_field(hook: &mut HookConfig, key: &str, value: &str) {
    match key {
        "name" => hook.name = value.to_string(),
        "command" => hook.command = value.to_string(),
        "timeout" => {
            if let Ok(t) = value.parse::<u64>() {
                hook.timeout = t;
            }
        }
        "inject_on_failure" => {
            hook.inject_on_failure = value != "false";
        }
        "trigger_tools" => {
            // Parse "[write, edit, bash]" or "write, edit, bash"
            let cleaned = value.trim_start_matches('[').trim_end_matches(']');
            let tools: Vec<String> = cleaned.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            if !tools.is_empty() {
                hook.trigger_tools = Some(tools);
            }
        }
        _ => {}
    }
}

fn apply_list_field(config: &mut ProjectConfig, key: &str, items: &[String]) {
    match key {
        "validation_commands" => config.validation_commands = items.to_vec(),
        "allowed_tools" => config.allowed_tools = Some(items.to_vec()),
        _ => {}
    }
}

fn parse_thinking(s: &str) -> ThinkingLevel {
    match s.to_lowercase().as_str() {
        "minimal" => ThinkingLevel::Minimal,
        "low" => ThinkingLevel::Low,
        "medium" => ThinkingLevel::Medium,
        "high" => ThinkingLevel::High,
        _ => ThinkingLevel::Off,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rho_md_with_all_fields() {
        let content = "\
---
model: claude-opus-4-6
thinking: medium
compact_threshold: 0.8
validation_commands:
  - cargo test --quiet
  - cargo clippy --quiet -- -D warnings
---

# Project Instructions

This is a Rust workspace. Always run `cargo test` after changes.
";
        let config = parse_rho_md(content, PathBuf::from("RHO.md"));
        assert_eq!(config.model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(config.thinking, Some(ThinkingLevel::Medium));
        assert_eq!(config.compact_threshold, Some(0.8));
        assert_eq!(config.validation_commands.len(), 2);
        assert_eq!(config.validation_commands[0], "cargo test --quiet");
        assert_eq!(
            config.validation_commands[1],
            "cargo clippy --quiet -- -D warnings"
        );
        assert!(config.system_prompt_append.unwrap().contains("Rust workspace"));
    }

    #[test]
    fn parse_rho_md_no_frontmatter() {
        let content = "# Just a markdown file\n\nNo frontmatter here.";
        let config = parse_rho_md(content, PathBuf::from("RHO.md"));
        assert!(config.model.is_none());
        assert!(config.system_prompt_append.unwrap().contains("Just a markdown file"));
    }

    #[test]
    fn parse_claude_md_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let claude_md = tmp.path().join("CLAUDE.md");
        std::fs::write(&claude_md, "# Instructions\nAlways test.").unwrap();

        let config = load_project_config(tmp.path());
        assert_eq!(config.source.as_deref(), Some(claude_md.as_path()));
        assert!(config.system_prompt_append.unwrap().contains("Always test"));
    }

    #[test]
    fn parse_rho_md_inline_tools() {
        let content = "---\nallowed_tools: read, grep, find\n---\n";
        let config = parse_rho_md(content, PathBuf::from("RHO.md"));
        let tools = config.allowed_tools.unwrap();
        assert_eq!(tools, vec!["read", "grep", "find"]);
    }

    #[test]
    fn parse_rho_md_agents_block() {
        let content = "\
---
max_agent_depth: 3
agents:
  - name: researcher
    tools: read,grep,find
    model: claude-haiku
    description: Research agent for code analysis
  - name: test-runner
    tools: bash,read
    description: Runs tests and reports results
---

# Instructions
";
        let config = parse_rho_md(content, PathBuf::from("RHO.md"));
        assert_eq!(config.max_agent_depth, Some(3));
        assert_eq!(config.agents.len(), 2);

        assert_eq!(config.agents[0].name, "researcher");
        assert_eq!(config.agents[0].tools.as_deref(), Some("read,grep,find"));
        assert_eq!(config.agents[0].model.as_deref(), Some("claude-haiku"));
        assert_eq!(
            config.agents[0].description.as_deref(),
            Some("Research agent for code analysis")
        );

        assert_eq!(config.agents[1].name, "test-runner");
        assert_eq!(config.agents[1].tools.as_deref(), Some("bash,read"));
        assert!(config.agents[1].model.is_none());
        assert_eq!(
            config.agents[1].description.as_deref(),
            Some("Runs tests and reports results")
        );
    }

    #[test]
    fn parse_rho_md_no_agents_backwards_compat() {
        let content = "\
---
model: claude-sonnet
---

# Instructions
";
        let config = parse_rho_md(content, PathBuf::from("RHO.md"));
        assert!(config.agents.is_empty());
        assert!(config.max_agent_depth.is_none());
    }

    #[test]
    fn load_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let config = load_project_config(tmp.path());
        assert!(config.model.is_none());
        assert!(config.system_prompt_append.is_none());
        assert!(config.source.is_none());
    }
}
