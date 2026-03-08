use std::path::{Path, PathBuf};

use crate::types::ThinkingLevel;

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
    pub auto_commit: bool,
    pub planning: bool,
    pub source: Option<PathBuf>,
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
            compact_threshold: Some(0.8),
            memories: true,
            auto_commit: false,
            planning: false,
            source: None,
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

    // Try AGENTS.md (ecosystem standard: Codex, Cursor, Copilot, Amp, etc.)
    let agents_md = cwd.join("AGENTS.md");
    if agents_md.is_file() {
        if let Ok(content) = std::fs::read_to_string(&agents_md) {
            return parse_rho_md(&content, agents_md);
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
    let mut in_list: Option<&str> = None;
    let mut list_items: Vec<String> = Vec::new();

    for line in frontmatter.lines() {
        let trimmed_line = line.trim();

        // Check if this is a list item for current key
        if let Some(key) = in_list {
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
                // Could be start of a list
                in_list = Some(key);
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
                "auto_commit" => {
                    config.auto_commit = value == "true";
                }
                "planning" => {
                    config.planning = value == "true";
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
    if let Some(key) = in_list {
        apply_list_field(&mut config, key, &list_items);
    }

    config
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
    fn load_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let config = load_project_config(tmp.path());
        assert!(config.model.is_none());
        assert!(config.system_prompt_append.is_none());
        assert!(config.source.is_none());
        // Default compaction should be enabled
        assert_eq!(config.compact_threshold, Some(0.8));
    }

    #[test]
    fn load_agents_md() {
        let tmp = tempfile::tempdir().unwrap();
        let agents_md = tmp.path().join("AGENTS.md");
        std::fs::write(
            &agents_md,
            "---\nmodel: claude-sonnet\n---\n\n# Agent Instructions\nBe helpful.",
        )
        .unwrap();

        let config = load_project_config(tmp.path());
        assert_eq!(config.source.as_deref(), Some(agents_md.as_path()));
        assert_eq!(config.model.as_deref(), Some("claude-sonnet"));
        assert!(config
            .system_prompt_append
            .unwrap()
            .contains("Be helpful"));
    }

    #[test]
    fn parse_auto_commit_true() {
        let content = "---\nauto_commit: true\n---\n";
        let config = parse_rho_md(content, PathBuf::from("RHO.md"));
        assert!(config.auto_commit);
    }

    #[test]
    fn parse_auto_commit_false() {
        let content = "---\nauto_commit: false\n---\n";
        let config = parse_rho_md(content, PathBuf::from("RHO.md"));
        assert!(!config.auto_commit);
    }

    #[test]
    fn auto_commit_defaults_to_false() {
        let content = "---\nmodel: claude-sonnet\n---\n";
        let config = parse_rho_md(content, PathBuf::from("RHO.md"));
        assert!(!config.auto_commit);
    }

    #[test]
    fn parse_planning_true() {
        let content = "---\nplanning: true\n---\n";
        let config = parse_rho_md(content, PathBuf::from("RHO.md"));
        assert!(config.planning);
    }

    #[test]
    fn parse_planning_false() {
        let content = "---\nplanning: false\n---\n";
        let config = parse_rho_md(content, PathBuf::from("RHO.md"));
        assert!(!config.planning);
    }

    #[test]
    fn planning_defaults_to_false() {
        let content = "---\nmodel: claude-sonnet\n---\n";
        let config = parse_rho_md(content, PathBuf::from("RHO.md"));
        assert!(!config.planning);
    }

    #[test]
    fn agents_md_takes_precedence_over_claude_md() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("AGENTS.md"),
            "---\nmodel: from-agents\n---\n",
        )
        .unwrap();
        std::fs::write(tmp.path().join("CLAUDE.md"), "# From CLAUDE.md").unwrap();

        let config = load_project_config(tmp.path());
        assert_eq!(config.model.as_deref(), Some("from-agents"));
    }
}
