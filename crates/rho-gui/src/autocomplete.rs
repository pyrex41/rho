use std::path::Path;

use rho_core::memories::MemoryMetadata;
use rho_core::skills::SkillMetadata;

#[derive(Debug, Clone, Default)]
pub struct AutocompleteState {
    pub active: bool,
    pub trigger: Option<AutocompleteTrigger>,
    pub suggestions: Vec<Suggestion>,
    pub selected: usize,
}

#[derive(Debug, Clone)]
pub enum AutocompleteTrigger {
    Skill { query: String, start: usize },
    File { query: String, start: usize },
}

#[derive(Debug, Clone)]
pub struct Suggestion {
    pub display: String,
    pub completion: String,
    pub is_directory: bool,
}

impl AutocompleteState {
    pub fn dismiss(&mut self) {
        self.active = false;
        self.trigger = None;
        self.suggestions.clear();
        self.selected = 0;
    }

    pub fn navigate(&mut self, delta: i32) {
        if self.suggestions.is_empty() {
            return;
        }
        let len = self.suggestions.len() as i32;
        self.selected = ((self.selected as i32 + delta).rem_euclid(len)) as usize;
    }
}

/// Scan backwards from the end of `input` to find an active `/` or `@` trigger.
///
/// A trigger is valid when preceded by start-of-string or whitespace,
/// and the query after it contains no spaces.
pub fn detect_trigger(input: &str) -> Option<AutocompleteTrigger> {
    // Find the start of the last whitespace-delimited token
    let token_start = input
        .rfind(|c: char| c.is_whitespace())
        .map(|pos| pos + 1)
        .unwrap_or(0);

    let token = &input[token_start..];
    if token.is_empty() {
        return None;
    }

    let first = token.as_bytes()[0];
    match first {
        b'/' => {
            let query = token[1..].to_string();
            // Skill queries must not contain `/` or spaces
            if query.contains('/') || query.contains(' ') {
                return None;
            }
            Some(AutocompleteTrigger::Skill {
                query,
                start: token_start,
            })
        }
        b'@' => {
            let query = token[1..].to_string();
            // File queries can contain `/` (for paths) but not spaces
            if query.contains(' ') {
                return None;
            }
            Some(AutocompleteTrigger::File {
                query,
                start: token_start,
            })
        }
        _ => None,
    }
}

/// List file/directory suggestions for the `@` trigger.
pub fn list_file_suggestions(cwd: &Path, query: &str) -> Vec<Suggestion> {
    // Split query at last `/` into dir_part and filter
    let (dir_part, filter) = match query.rfind('/') {
        Some(pos) => (&query[..=pos], &query[pos + 1..]),
        None => ("", query),
    };

    let search_dir = if dir_part.is_empty() {
        cwd.to_path_buf()
    } else {
        cwd.join(dir_part)
    };

    let entries = match std::fs::read_dir(&search_dir) {
        Ok(rd) => rd,
        Err(_) => return Vec::new(),
    };

    let filter_lower = filter.to_lowercase();

    let mut suggestions: Vec<Suggestion> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            // Skip hidden files
            if name.starts_with('.') {
                return None;
            }
            // Filter by substring
            if !filter_lower.is_empty() && !name.to_lowercase().contains(&filter_lower) {
                return None;
            }
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            let completion = if is_dir {
                format!("@{}{}{}/", dir_part, name, "")
            } else {
                format!("@{}{}", dir_part, name)
            };
            let display = if is_dir {
                format!("{}/", name)
            } else {
                name.clone()
            };
            Some(Suggestion {
                display,
                completion,
                is_directory: is_dir,
            })
        })
        .collect();

    // Sort: directories first, then alphabetical
    suggestions.sort_by(|a, b| {
        b.is_directory
            .cmp(&a.is_directory)
            .then(a.display.to_lowercase().cmp(&b.display.to_lowercase()))
    });

    suggestions.truncate(20);
    suggestions
}

/// List skill, command, and memory suggestions for the `/` trigger.
pub fn list_skill_suggestions(
    skills: &[SkillMetadata],
    memories: &[MemoryMetadata],
    query: &str,
    cwd: &Path,
) -> Vec<Suggestion> {
    let query_lower = query.to_lowercase();
    let mut suggestions = Vec::new();

    // Built-in commands first
    let commands = rho_core::commands::all_commands(cwd);
    for cmd in &commands {
        if query_lower.is_empty() || cmd.name.to_lowercase().contains(&query_lower) {
            suggestions.push(Suggestion {
                display: format!("/{} — {}", cmd.name, cmd.description),
                completion: format!("/{}", cmd.name),
                is_directory: false,
            });
        }
    }

    // Then skills
    for s in skills {
        if query_lower.is_empty() || s.name.to_lowercase().contains(&query_lower) {
            suggestions.push(Suggestion {
                display: format!("{} — {}", s.name, s.description),
                completion: format!("/{}", s.name),
                is_directory: false,
            });
        }
    }

    // Then memories
    for m in memories {
        if query_lower.is_empty() || m.name.to_lowercase().contains(&query_lower) {
            let tag_str = if m.tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", m.tags.join(", "))
            };
            suggestions.push(Suggestion {
                display: format!("{} — {}{}", m.name, m.description, tag_str),
                completion: format!("/{}", m.name),
                is_directory: false,
            });
        }
    }

    suggestions
}

/// Resolve `/skill`, `/memory`, and `@file` references in the input.
///
/// Returns `(display_text, resolved_text)`:
/// - `display_text` is the original input (shown in chat)
/// - `resolved_text` has references replaced with content blocks (sent to agent)
pub fn resolve_references(
    input: &str,
    cwd: &Path,
    skills: &[SkillMetadata],
    memories: &[MemoryMetadata],
) -> (String, String) {
    let display_text = input.to_string();
    let mut resolved = input.to_string();

    // Resolve `/skill` references — find tokens starting with `/` preceded by SOL or whitespace
    for skill in skills {
        let token = format!("/{}", skill.name);
        // Replace each occurrence that is a word boundary match
        let mut result = String::new();
        let mut remaining = resolved.as_str();
        while let Some(pos) = remaining.find(&token) {
            // Check preceding char is SOL or whitespace
            let valid_start = if pos == 0 {
                true
            } else {
                remaining[..pos].ends_with(char::is_whitespace)
            };
            // Check following char is EOL or whitespace
            let end = pos + token.len();
            let valid_end =
                end >= remaining.len() || remaining[end..].starts_with(char::is_whitespace);

            if valid_start && valid_end {
                result.push_str(&remaining[..pos]);
                // Read skill content
                let content = std::fs::read_to_string(&skill.path).unwrap_or_default();
                result.push_str(&format!(
                    "<skill name=\"{}\">\n{}\n</skill>",
                    skill.name, content
                ));
                remaining = &remaining[end..];
            } else {
                result.push_str(&remaining[..end]);
                remaining = &remaining[end..];
            }
        }
        result.push_str(remaining);
        resolved = result;
    }

    // Resolve `/memory` references
    for memory in memories {
        let token = format!("/{}", memory.name);
        let mut result = String::new();
        let mut remaining = resolved.as_str();
        while let Some(pos) = remaining.find(&token) {
            let valid_start = if pos == 0 {
                true
            } else {
                remaining[..pos].ends_with(char::is_whitespace)
            };
            let end = pos + token.len();
            let valid_end =
                end >= remaining.len() || remaining[end..].starts_with(char::is_whitespace);

            if valid_start && valid_end {
                result.push_str(&remaining[..pos]);
                if let Some(content) = rho_core::memories::load_memory_content(memory) {
                    result.push_str(&format!(
                        "<memory name=\"{}\">\n{}\n</memory>",
                        memory.name, content
                    ));
                }
                remaining = &remaining[end..];
            } else {
                result.push_str(&remaining[..end]);
                remaining = &remaining[end..];
            }
        }
        result.push_str(remaining);
        resolved = result;
    }

    // Resolve `@file` references
    let mut result = String::new();
    let mut remaining = resolved.as_str();
    while let Some(pos) = remaining.find('@') {
        let valid_start = if pos == 0 {
            true
        } else {
            remaining[..pos].ends_with(char::is_whitespace)
        };

        if !valid_start {
            result.push_str(&remaining[..pos + 1]);
            remaining = &remaining[pos + 1..];
            continue;
        }

        // Extract the file path (until whitespace or EOL)
        let after_at = &remaining[pos + 1..];
        let path_end = after_at.find(char::is_whitespace).unwrap_or(after_at.len());
        let file_path = &after_at[..path_end];

        if file_path.is_empty() {
            result.push_str(&remaining[..pos + 1]);
            remaining = &remaining[pos + 1..];
            continue;
        }

        let full_path = cwd.join(file_path);
        if full_path.is_file() {
            result.push_str(&remaining[..pos]);
            let content = std::fs::read_to_string(&full_path).unwrap_or_default();
            result.push_str(&format!(
                "<file path=\"{}\">\n{}\n</file>",
                file_path, content
            ));
            remaining = &remaining[pos + 1 + path_end..];
        } else {
            // Not a valid file, keep as-is
            result.push_str(&remaining[..pos + 1 + path_end]);
            remaining = &remaining[pos + 1 + path_end..];
        }
    }
    result.push_str(remaining);

    (display_text, result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn detect_trigger_slash_at_start() {
        let t = detect_trigger("/calc").unwrap();
        match t {
            AutocompleteTrigger::Skill { query, start } => {
                assert_eq!(query, "calc");
                assert_eq!(start, 0);
            }
            _ => panic!("expected Skill trigger"),
        }
    }

    #[test]
    fn detect_trigger_slash_after_space() {
        let t = detect_trigger("hello /calc").unwrap();
        match t {
            AutocompleteTrigger::Skill { query, start } => {
                assert_eq!(query, "calc");
                assert_eq!(start, 6);
            }
            _ => panic!("expected Skill trigger"),
        }
    }

    #[test]
    fn detect_trigger_at_sign() {
        let t = detect_trigger("@src/main").unwrap();
        match t {
            AutocompleteTrigger::File { query, start } => {
                assert_eq!(query, "src/main");
                assert_eq!(start, 0);
            }
            _ => panic!("expected File trigger"),
        }
    }

    #[test]
    fn detect_trigger_none_mid_word() {
        // `/` not preceded by whitespace
        assert!(detect_trigger("foo/bar").is_none());
    }

    #[test]
    fn detect_trigger_none_with_space_in_query() {
        // Query has a space — not a valid trigger
        assert!(detect_trigger("/hello world").is_none());
    }

    #[test]
    fn detect_trigger_empty_query() {
        let t = detect_trigger("/").unwrap();
        match t {
            AutocompleteTrigger::Skill { query, start } => {
                assert_eq!(query, "");
                assert_eq!(start, 0);
            }
            _ => panic!("expected Skill trigger"),
        }
    }

    #[test]
    fn detect_trigger_at_empty_query() {
        let t = detect_trigger("@").unwrap();
        match t {
            AutocompleteTrigger::File { query, start } => {
                assert_eq!(query, "");
                assert_eq!(start, 0);
            }
            _ => panic!("expected File trigger"),
        }
    }

    #[test]
    fn list_file_suggestions_basic() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("hello.rs"), "").unwrap();
        fs::write(tmp.path().join("world.txt"), "").unwrap();
        fs::create_dir(tmp.path().join("subdir")).unwrap();

        let suggestions = list_file_suggestions(tmp.path(), "");
        assert_eq!(suggestions.len(), 3);
        // Directories first
        assert!(suggestions[0].is_directory);
        assert_eq!(suggestions[0].display, "subdir/");
    }

    #[test]
    fn list_file_suggestions_filter() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("hello.rs"), "").unwrap();
        fs::write(tmp.path().join("world.txt"), "").unwrap();

        let suggestions = list_file_suggestions(tmp.path(), "hel");
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].display, "hello.rs");
    }

    #[test]
    fn list_file_suggestions_skips_hidden() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".hidden"), "").unwrap();
        fs::write(tmp.path().join("visible"), "").unwrap();

        let suggestions = list_file_suggestions(tmp.path(), "");
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].display, "visible");
    }

    #[test]
    fn list_file_suggestions_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("src");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("main.rs"), "").unwrap();
        fs::write(sub.join("lib.rs"), "").unwrap();

        let suggestions = list_file_suggestions(tmp.path(), "src/");
        assert_eq!(suggestions.len(), 2);
    }

    #[test]
    fn list_skill_suggestions_basic() {
        let skills = vec![
            SkillMetadata {
                name: "calculator".into(),
                description: "Math operations".into(),
                path: "/tmp/calc/SKILL.md".into(),
                ..Default::default()
            },
            SkillMetadata {
                name: "formatter".into(),
                description: "Code formatting".into(),
                path: "/tmp/fmt/SKILL.md".into(),
                ..Default::default()
            },
        ];

        let tmp = tempfile::tempdir().unwrap();
        let suggestions = list_skill_suggestions(&skills, &[], "", tmp.path());
        // 2 skills + 5 built-in commands
        assert!(suggestions.len() >= 2);

        let suggestions = list_skill_suggestions(&skills, &[], "calc", tmp.path());
        assert!(suggestions.iter().any(|s| s.completion == "/calculator"));
    }

    #[test]
    fn resolve_references_skill() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_file = tmp.path().join("SKILL.md");
        fs::write(
            &skill_file,
            "---\nname: calc\ndescription: Math\n---\nDo math.",
        )
        .unwrap();

        let skills = vec![SkillMetadata {
            name: "calc".into(),
            description: "Math".into(),
            path: skill_file,
            ..Default::default()
        }];

        let (display, resolved) = resolve_references("/calc", tmp.path(), &skills, &[]);
        assert_eq!(display, "/calc");
        assert!(resolved.contains("<skill name=\"calc\">"));
        assert!(resolved.contains("Do math."));
    }

    #[test]
    fn resolve_references_file() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("test.txt"), "file content here").unwrap();

        let (display, resolved) = resolve_references("@test.txt", tmp.path(), &[], &[]);
        assert_eq!(display, "@test.txt");
        assert!(resolved.contains("<file path=\"test.txt\">"));
        assert!(resolved.contains("file content here"));
    }

    #[test]
    fn resolve_references_nonexistent_file() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, resolved) = resolve_references("@nofile.txt", tmp.path(), &[], &[]);
        assert_eq!(resolved, "@nofile.txt");
    }

    #[test]
    fn resolve_references_mixed() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_file = tmp.path().join("SKILL.md");
        fs::write(&skill_file, "skill content").unwrap();
        fs::write(tmp.path().join("data.txt"), "data content").unwrap();

        let skills = vec![SkillMetadata {
            name: "tool".into(),
            description: "A tool".into(),
            path: skill_file,
            ..Default::default()
        }];

        let input = "Use /tool and read @data.txt please";
        let (display, resolved) = resolve_references(input, tmp.path(), &skills, &[]);
        assert_eq!(display, input);
        assert!(resolved.contains("<skill name=\"tool\">"));
        assert!(resolved.contains("<file path=\"data.txt\">"));
        assert!(resolved.contains("please"));
    }

    #[test]
    fn resolve_references_edited_skill_not_resolved() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_file = tmp.path().join("SKILL.md");
        fs::write(&skill_file, "skill content").unwrap();

        let skills = vec![SkillMetadata {
            name: "calculator".into(),
            description: "Math".into(),
            path: skill_file,
            ..Default::default()
        }];

        // Partial name after editing — should NOT resolve
        let (_, resolved) = resolve_references("/calc", tmp.path(), &skills, &[]);
        assert_eq!(resolved, "/calc");

        // Fully removed — should NOT resolve
        let (_, resolved) = resolve_references("hello world", tmp.path(), &skills, &[]);
        assert_eq!(resolved, "hello world");

        // Exact name — should resolve
        let (_, resolved) = resolve_references("/calculator", tmp.path(), &skills, &[]);
        assert!(resolved.contains("<skill name=\"calculator\">"));
    }

    #[test]
    fn navigate_wraps_around() {
        let mut state = AutocompleteState {
            active: true,
            trigger: None,
            suggestions: vec![
                Suggestion {
                    display: "a".into(),
                    completion: "a".into(),
                    is_directory: false,
                },
                Suggestion {
                    display: "b".into(),
                    completion: "b".into(),
                    is_directory: false,
                },
                Suggestion {
                    display: "c".into(),
                    completion: "c".into(),
                    is_directory: false,
                },
            ],
            selected: 0,
        };

        state.navigate(-1);
        assert_eq!(state.selected, 2);

        state.navigate(1);
        assert_eq!(state.selected, 0);

        state.navigate(1);
        assert_eq!(state.selected, 1);
    }
}
