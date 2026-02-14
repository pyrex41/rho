use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

/// Parse YAML frontmatter between `---` markers, extracting `name` and `description`.
fn parse_frontmatter(content: &str) -> Option<(String, String)> {
    let content = content.trim_start();
    if !content.starts_with("---") {
        return None;
    }
    let after_first = &content[3..];
    let end = after_first.find("\n---")?;
    let block = &after_first[..end];

    let mut name = None;
    let mut description = None;
    for line in block.lines() {
        if let Some(val) = line.strip_prefix("name:") {
            name = Some(val.trim().to_string());
        } else if let Some(val) = line.strip_prefix("description:") {
            description = Some(val.trim().to_string());
        }
    }

    Some((name?, description?))
}

/// Discover skills in the given directories.
///
/// For each directory, lists subdirectories and checks for a `SKILL.md` file.
/// The skill name in the frontmatter must match the directory name.
pub fn discover_skills(dirs: &[PathBuf]) -> Vec<SkillMetadata> {
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
                let Some((name, description)) = parse_frontmatter(&content) else {
                    continue;
                };
                let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name != dir_name {
                    continue;
                }
                skills.push(SkillMetadata {
                    name,
                    description,
                    path: std::fs::canonicalize(&skill_file).unwrap_or(skill_file),
                });
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
pub fn format_skills_prompt(skills: &[SkillMetadata]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut out = String::from("<available_skills>\n");
    for skill in skills {
        out.push_str("  <skill>\n");
        out.push_str(&format!("    <name>{}</name>\n", skill.name));
        out.push_str(&format!("    <description>{}</description>\n", skill.description));
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
        let (name, desc) = parse_frontmatter(content).unwrap();
        assert_eq!(name, "hello");
        assert_eq!(desc, "A test skill.");
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
        let (name, desc) = parse_frontmatter(content).unwrap();
        assert_eq!(name, "spaced");
        assert_eq!(desc, "A description");
    }

    #[test]
    fn format_empty_skills() {
        assert_eq!(format_skills_prompt(&[]), "");
    }

    #[test]
    fn format_single_skill() {
        let skills = vec![SkillMetadata {
            name: "pdf".into(),
            description: "Processes PDFs.".into(),
            path: PathBuf::from("/home/user/.skills/pdf/SKILL.md"),
        }];
        let xml = format_skills_prompt(&skills);
        assert!(xml.contains("<available_skills>"));
        assert!(xml.contains("<name>pdf</name>"));
        assert!(xml.contains("<description>Processes PDFs.</description>"));
        assert!(xml.contains("<location>/home/user/.skills/pdf/SKILL.md</location>"));
        assert!(xml.contains("</available_skills>"));
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
