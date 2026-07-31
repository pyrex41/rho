use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct MemoryMetadata {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub path: PathBuf,
}

/// Parse YAML frontmatter, extracting `name`, `description`, and `tags`.
fn parse_frontmatter(content: &str) -> Option<(String, String, Vec<String>)> {
    let content = content.trim_start();
    if !content.starts_with("---") {
        return None;
    }
    let after_first = &content[3..];
    let end = after_first.find("\n---")?;
    let block = &after_first[..end];

    let mut name = None;
    let mut description = None;
    let mut tags = Vec::new();

    for line in block.lines() {
        if let Some(val) = line.strip_prefix("name:") {
            name = Some(val.trim().to_string());
        } else if let Some(val) = line.strip_prefix("description:") {
            description = Some(val.trim().to_string());
        } else if let Some(val) = line.strip_prefix("tags:") {
            // Parse inline YAML list: [tag1, tag2, tag3]
            let val = val.trim();
            if val.starts_with('[') && val.ends_with(']') {
                tags = val[1..val.len() - 1]
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }
    }

    Some((name?, description?, tags))
}

/// Discover memories in the given directories.
///
/// Each `.md` file in a memory directory is a memory.
/// The file stem must match the `name` field in frontmatter.
pub fn discover_memories(dirs: &[PathBuf]) -> Vec<MemoryMetadata> {
    let mut memories = Vec::new();
    for dir in dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let ext = path.extension().and_then(|e| e.to_str());
                if ext != Some("md") {
                    continue;
                }
                let Ok(content) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let Some((name, description, tags)) = parse_frontmatter(&content) else {
                    continue;
                };
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                if name != stem {
                    continue;
                }
                memories.push(MemoryMetadata {
                    name,
                    description,
                    tags,
                    path: std::fs::canonicalize(&path).unwrap_or(path),
                });
            }
        }
    }
    memories.sort_by(|a, b| a.name.cmp(&b.name));
    memories
}

/// Build the default memory discovery directories.
///
/// Project-local: `.rho/memories/`
/// Global: `~/.rho/memories/`
pub fn default_memory_dirs(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![cwd.join(".rho/memories")];
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".rho/memories"));
    }
    dirs
}

/// Format discovered memories as an XML block for system prompt injection.
pub fn format_memories_prompt(memories: &[MemoryMetadata]) -> String {
    if memories.is_empty() {
        return String::new();
    }
    let mut out = String::from("<available_memories>\n");
    for mem in memories {
        out.push_str("  <memory>\n");
        out.push_str(&format!("    <name>{}</name>\n", mem.name));
        out.push_str(&format!(
            "    <description>{}</description>\n",
            mem.description
        ));
        if !mem.tags.is_empty() {
            out.push_str(&format!("    <tags>{}</tags>\n", mem.tags.join(", ")));
        }
        out.push_str("  </memory>\n");
    }
    out.push_str("</available_memories>\n");
    out.push_str(
        "To invoke a memory, the user types /memory-name. \
         To create a new memory, write a .md file to .rho/memories/ with frontmatter (name, description, tags) and body content.",
    );
    out
}

/// Load the content body of a memory (strips frontmatter).
pub fn load_memory_content(memory: &MemoryMetadata) -> Option<String> {
    let content = std::fs::read_to_string(&memory.path).ok()?;
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return Some(content);
    }
    let after_first = &trimmed[3..];
    let end = after_first.find("\n---")?;
    let body_start = 3 + end + 4; // "---" + frontmatter + "\n---"
    let body = trimmed[body_start..].trim_start_matches('\n');
    Some(body.to_string())
}

/// Search memories by case-insensitive substring match on name, description, and tags.
pub fn search_memories<'a>(memories: &'a [MemoryMetadata], query: &str) -> Vec<&'a MemoryMetadata> {
    let query_lower = query.to_lowercase();
    memories
        .iter()
        .filter(|m| {
            m.name.to_lowercase().contains(&query_lower)
                || m.description.to_lowercase().contains(&query_lower)
                || m.tags
                    .iter()
                    .any(|t| t.to_lowercase().contains(&query_lower))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parse_frontmatter_basic() {
        let content =
            "---\nname: rust-testing\ndescription: Testing patterns\ntags: [rust, testing]\n---\nBody.";
        let (name, desc, tags) = parse_frontmatter(content).unwrap();
        assert_eq!(name, "rust-testing");
        assert_eq!(desc, "Testing patterns");
        assert_eq!(tags, vec!["rust", "testing"]);
    }

    #[test]
    fn parse_frontmatter_no_tags() {
        let content = "---\nname: simple\ndescription: A simple memory\n---\nContent.";
        let (name, desc, tags) = parse_frontmatter(content).unwrap();
        assert_eq!(name, "simple");
        assert_eq!(desc, "A simple memory");
        assert!(tags.is_empty());
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
    fn discover_memories_from_dir() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("my-memory.md"),
            "---\nname: my-memory\ndescription: Test memory\ntags: [test]\n---\nContent here.",
        )
        .unwrap();

        let memories = discover_memories(&[tmp.path().to_path_buf()]);
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].name, "my-memory");
        assert_eq!(memories[0].description, "Test memory");
        assert_eq!(memories[0].tags, vec!["test"]);
    }

    #[test]
    fn discover_memories_name_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("actual-file.md"),
            "---\nname: wrong-name\ndescription: Oops\n---\n",
        )
        .unwrap();

        let memories = discover_memories(&[tmp.path().to_path_buf()]);
        assert!(memories.is_empty());
    }

    #[test]
    fn discover_memories_missing_dir() {
        let memories = discover_memories(&[PathBuf::from("/nonexistent/memories")]);
        assert!(memories.is_empty());
    }

    #[test]
    fn discover_memories_skips_non_md() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("notes.txt"),
            "---\nname: notes\ndescription: Notes\n---\nNotes.",
        )
        .unwrap();

        let memories = discover_memories(&[tmp.path().to_path_buf()]);
        assert!(memories.is_empty());
    }

    #[test]
    fn format_empty_memories() {
        assert_eq!(format_memories_prompt(&[]), "");
    }

    #[test]
    fn format_memories_with_tags() {
        let memories = vec![MemoryMetadata {
            name: "rust-testing".into(),
            description: "Testing patterns".into(),
            tags: vec!["rust".into(), "testing".into()],
            path: PathBuf::from("/tmp/memories/rust-testing.md"),
        }];
        let xml = format_memories_prompt(&memories);
        assert!(xml.contains("<available_memories>"));
        assert!(xml.contains("<name>rust-testing</name>"));
        assert!(xml.contains("<tags>rust, testing</tags>"));
        assert!(xml.contains("</available_memories>"));
    }

    #[test]
    fn load_memory_content_strips_frontmatter() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.md");
        fs::write(
            &path,
            "---\nname: test\ndescription: Test\ntags: [test]\n---\nActual content here.",
        )
        .unwrap();

        let mem = MemoryMetadata {
            name: "test".into(),
            description: "Test".into(),
            tags: vec!["test".into()],
            path,
        };
        let content = load_memory_content(&mem).unwrap();
        assert_eq!(content, "Actual content here.");
    }

    #[test]
    fn search_memories_by_name() {
        let memories = vec![
            MemoryMetadata {
                name: "rust-testing".into(),
                description: "Testing".into(),
                tags: vec![],
                path: PathBuf::new(),
            },
            MemoryMetadata {
                name: "python-debugging".into(),
                description: "Debugging".into(),
                tags: vec!["python".into()],
                path: PathBuf::new(),
            },
        ];
        let results = search_memories(&memories, "rust");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "rust-testing");
    }

    #[test]
    fn search_memories_by_tag() {
        let memories = vec![MemoryMetadata {
            name: "something".into(),
            description: "Other".into(),
            tags: vec!["rust".into(), "async".into()],
            path: PathBuf::new(),
        }];
        let results = search_memories(&memories, "async");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_memories_case_insensitive() {
        let memories = vec![MemoryMetadata {
            name: "RustTesting".into(),
            description: "Testing".into(),
            tags: vec![],
            path: PathBuf::new(),
        }];
        let results = search_memories(&memories, "rusttesting");
        assert_eq!(results.len(), 1);
    }
}
