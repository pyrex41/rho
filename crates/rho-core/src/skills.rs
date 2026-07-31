use std::path::{Path, PathBuf};

/// How a skill should be executed when invoked.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum SkillExecution {
    /// Run inline in the current conversation context (default).
    #[default]
    Inline,
    /// Fork into a sub-agent via the task tool.
    Fork,
}

#[derive(Debug, Clone)]
pub struct SkillDef {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    /// Guidance for the model on when this skill is appropriate.
    pub when_to_use: Option<String>,
    /// Restrict which tools the skill can use (for fork execution).
    pub allowed_tools: Option<Vec<String>>,
    /// Override model for this skill (for fork execution).
    pub model: Option<String>,
    /// Whether to run inline or fork to a sub-agent.
    pub execution: SkillExecution,
    /// Tags for categorization and search.
    pub tags: Option<Vec<String>>,
}

impl Default for SkillDef {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            path: PathBuf::new(),
            when_to_use: None,
            allowed_tools: None,
            model: None,
            execution: SkillExecution::default(),
            tags: None,
        }
    }
}

/// Backwards-compatible type alias.
pub type SkillMetadata = SkillDef;

/// Parse a comma-separated inline list like `[read, grep, find]` or `read, grep, find`.
fn parse_inline_list(val: &str) -> Vec<String> {
    let trimmed = val.trim();
    let inner = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed);
    inner
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Parse YAML frontmatter between `---` markers.
fn parse_frontmatter(content: &str) -> Option<SkillDef> {
    let content = content.trim_start();
    if !content.starts_with("---") {
        return None;
    }
    let after_first = &content[3..];
    let end = after_first.find("\n---")?;
    let block = &after_first[..end];

    let mut name = None;
    let mut description = None;
    let mut when_to_use = None;
    let mut allowed_tools = None;
    let mut model = None;
    let mut execution = SkillExecution::default();
    let mut tags = None;

    for line in block.lines() {
        if let Some(val) = line.strip_prefix("name:") {
            name = Some(val.trim().to_string());
        } else if let Some(val) = line.strip_prefix("description:") {
            description = Some(val.trim().to_string());
        } else if let Some(val) = line.strip_prefix("when_to_use:") {
            let v = val.trim().to_string();
            if !v.is_empty() {
                when_to_use = Some(v);
            }
        } else if let Some(val) = line.strip_prefix("allowed_tools:") {
            let list = parse_inline_list(val);
            if !list.is_empty() {
                allowed_tools = Some(list);
            }
        } else if let Some(val) = line.strip_prefix("model:") {
            let v = val.trim().to_string();
            if !v.is_empty() {
                model = Some(v);
            }
        } else if let Some(val) = line.strip_prefix("execution:") {
            match val.trim() {
                "fork" => execution = SkillExecution::Fork,
                _ => execution = SkillExecution::Inline,
            }
        } else if let Some(val) = line.strip_prefix("tags:") {
            let list = parse_inline_list(val);
            if !list.is_empty() {
                tags = Some(list);
            }
        }
    }

    Some(SkillDef {
        name: name?,
        description: description?,
        path: PathBuf::new(), // filled by caller
        when_to_use,
        allowed_tools,
        model,
        execution,
        tags,
    })
}

/// Discover skills in the given directories.
///
/// For each directory, lists subdirectories and checks for a `SKILL.md` file.
/// The skill name in the frontmatter must match the directory name.
pub fn discover_skills(dirs: &[PathBuf]) -> Vec<SkillDef> {
    let mut skills = Vec::new();
    for dir in dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let skill_file = path.join("SKILL.md");
                if !skill_file.is_file() {
                    continue;
                }
                let Ok(content) = std::fs::read_to_string(&skill_file) else {
                    continue;
                };
                let Some(mut skill) = parse_frontmatter(&content) else {
                    continue;
                };
                let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if skill.name != dir_name {
                    continue;
                }
                skill.path = std::fs::canonicalize(&skill_file).unwrap_or(skill_file);
                skills.push(skill);
            }
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// Build the default skill discovery directories.
///
/// Project-local: `.skills/`, `.claude/skills/`, `.opencode/skills/`
/// Global (home): `~/.skills/`, `~/.claude/skills/`, `~/.opencode/skills/`
pub fn default_skill_dirs(cwd: &Path) -> Vec<PathBuf> {
    let suffixes = [".skills", ".claude/skills", ".opencode/skills"];
    let mut dirs: Vec<PathBuf> = suffixes.iter().map(|s| cwd.join(s)).collect();
    if let Some(home) = dirs::home_dir() {
        dirs.extend(suffixes.iter().map(|s| home.join(s)));
    }
    dirs
}

/// Format discovered skills as an XML block for system prompt injection.
/// Returns an empty string if no skills are found.
pub fn format_skills_prompt(skills: &[SkillDef]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut out = String::from("<available_skills>\n");
    for skill in skills {
        out.push_str("  <skill>\n");
        out.push_str(&format!("    <name>{}</name>\n", skill.name));
        out.push_str(&format!(
            "    <description>{}</description>\n",
            skill.description
        ));
        if let Some(ref when) = skill.when_to_use {
            out.push_str(&format!("    <when_to_use>{}</when_to_use>\n", when));
        }
        if let Some(ref tags) = skill.tags {
            out.push_str(&format!("    <tags>{}</tags>\n", tags.join(", ")));
        }
        out.push_str(&format!(
            "    <location>{}</location>\n",
            skill.path.display()
        ));
        out.push_str("  </skill>\n");
    }
    out.push_str("</available_skills>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parse_frontmatter_basic() {
        let content = "---\nname: hello\ndescription: A test skill.\n---\nBody here.";
        let skill = parse_frontmatter(content).unwrap();
        assert_eq!(skill.name, "hello");
        assert_eq!(skill.description, "A test skill.");
        assert_eq!(skill.execution, SkillExecution::Inline);
        assert!(skill.when_to_use.is_none());
        assert!(skill.allowed_tools.is_none());
        assert!(skill.model.is_none());
        assert!(skill.tags.is_none());
    }

    #[test]
    fn parse_frontmatter_all_fields() {
        let content = "\
---
name: research
description: Deep codebase research
when_to_use: When the user asks to explore or understand code
allowed_tools: [read, grep, find]
model: claude-opus
execution: fork
tags: [research, exploration]
---
Skill body here.";
        let skill = parse_frontmatter(content).unwrap();
        assert_eq!(skill.name, "research");
        assert_eq!(skill.description, "Deep codebase research");
        assert_eq!(
            skill.when_to_use.as_deref(),
            Some("When the user asks to explore or understand code")
        );
        assert_eq!(
            skill.allowed_tools,
            Some(vec!["read".into(), "grep".into(), "find".into()])
        );
        assert_eq!(skill.model.as_deref(), Some("claude-opus"));
        assert_eq!(skill.execution, SkillExecution::Fork);
        assert_eq!(
            skill.tags,
            Some(vec!["research".into(), "exploration".into()])
        );
    }

    #[test]
    fn parse_frontmatter_missing_fields() {
        assert!(parse_frontmatter("---\nname: hello\n---\n").is_none());
        assert!(parse_frontmatter("---\ndescription: x\n---\n").is_none());
    }

    #[test]
    fn parse_frontmatter_no_markers() {
        assert!(parse_frontmatter("name: hello\ndescription: x").is_none());
    }

    #[test]
    fn parse_frontmatter_extra_whitespace() {
        let content = "---\nname:   spaced  \ndescription:  A description  \n---\n";
        let skill = parse_frontmatter(content).unwrap();
        assert_eq!(skill.name, "spaced");
        assert_eq!(skill.description, "A description");
    }

    #[test]
    fn parse_frontmatter_unknown_execution_defaults_to_inline() {
        let content = "---\nname: x\ndescription: y\nexecution: unknown\n---\n";
        let skill = parse_frontmatter(content).unwrap();
        assert_eq!(skill.execution, SkillExecution::Inline);
    }

    #[test]
    fn parse_inline_list_variants() {
        assert_eq!(parse_inline_list("[a, b, c]"), vec!["a", "b", "c"]);
        assert_eq!(parse_inline_list("a, b, c"), vec!["a", "b", "c"]);
        assert_eq!(parse_inline_list("  [  x ,  y  ]  "), vec!["x", "y"]);
        assert!(parse_inline_list("").is_empty());
        assert!(parse_inline_list("[]").is_empty());
    }

    #[test]
    fn format_empty_skills() {
        assert_eq!(format_skills_prompt(&[]), "");
    }

    #[test]
    fn format_single_skill() {
        let skills = vec![SkillDef {
            name: "pdf".into(),
            description: "Processes PDFs.".into(),
            path: PathBuf::from("/home/user/.skills/pdf/SKILL.md"),
            when_to_use: None,
            allowed_tools: None,
            model: None,
            execution: SkillExecution::Inline,
            tags: None,
        }];
        let xml = format_skills_prompt(&skills);
        assert!(xml.contains("<available_skills>"));
        assert!(xml.contains("<name>pdf</name>"));
        assert!(xml.contains("<description>Processes PDFs.</description>"));
        assert!(xml.contains("<location>/home/user/.skills/pdf/SKILL.md</location>"));
        assert!(xml.contains("</available_skills>"));
        assert!(!xml.contains("<when_to_use>"));
        assert!(!xml.contains("<tags>"));
    }

    #[test]
    fn format_skill_with_when_to_use_and_tags() {
        let skills = vec![SkillDef {
            name: "research".into(),
            description: "Explore code.".into(),
            path: PathBuf::from("/skills/research/SKILL.md"),
            when_to_use: Some("When exploring codebases".into()),
            allowed_tools: Some(vec!["read".into()]),
            model: Some("claude-opus".into()),
            execution: SkillExecution::Fork,
            tags: Some(vec!["research".into(), "code".into()]),
        }];
        let xml = format_skills_prompt(&skills);
        assert!(xml.contains("<when_to_use>When exploring codebases</when_to_use>"));
        assert!(xml.contains("<tags>research, code</tags>"));
        // Operational metadata should NOT appear in prompt
        assert!(!xml.contains("allowed_tools"));
        assert!(!xml.contains("model"));
        assert!(!xml.contains("execution"));
    }

    #[test]
    fn discover_skills_from_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("my-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: my-skill\ndescription: Does things.\n---\nInstructions.",
        )
        .unwrap();

        let skills = discover_skills(&[tmp.path().to_path_buf()]);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "my-skill");
        assert_eq!(skills[0].description, "Does things.");
        assert_eq!(skills[0].execution, SkillExecution::Inline);
    }

    #[test]
    fn discover_skills_with_full_frontmatter() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("research");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "\
---
name: research
description: Deep research
when_to_use: For exploration
allowed_tools: [read, grep]
model: claude-opus
execution: fork
tags: [research]
---
Do research.",
        )
        .unwrap();

        let skills = discover_skills(&[tmp.path().to_path_buf()]);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].execution, SkillExecution::Fork);
        assert_eq!(skills[0].model.as_deref(), Some("claude-opus"));
        assert_eq!(
            skills[0].allowed_tools,
            Some(vec!["read".into(), "grep".into()])
        );
    }

    #[test]
    fn discover_skills_name_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("actual-dir");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: wrong-name\ndescription: Oops.\n---\n",
        )
        .unwrap();

        let skills = discover_skills(&[tmp.path().to_path_buf()]);
        assert!(skills.is_empty());
    }

    #[test]
    fn discover_skills_missing_dir() {
        let skills = discover_skills(&[PathBuf::from("/nonexistent/path/skills")]);
        assert!(skills.is_empty());
    }
}
