use std::path::{Path, PathBuf};

/// Structured session memory maintained across the conversation.
/// Used as a compaction source (Tier 2) when context pressure is high.
#[derive(Debug, Clone, Default)]
pub struct SessionMemory {
    pub session_id: String,
    pub current_task: String,
    pub key_decisions: Vec<String>,
    pub files_modified: Vec<String>,
    pub errors_encountered: Vec<String>,
    pub general_summary: String,
    pub tool_call_count_at_update: usize,
    pub token_estimate_at_update: usize,
}

impl SessionMemory {
    /// Format session memory as a single text block suitable for injection as a conversation prefix.
    pub fn format_as_message(&self) -> String {
        let mut out = String::from("[Session Memory Summary]\n\n");

        if !self.current_task.is_empty() {
            out.push_str(&format!("## Current Task\n{}\n\n", self.current_task));
        }

        if !self.key_decisions.is_empty() {
            out.push_str("## Key Decisions\n");
            for d in &self.key_decisions {
                out.push_str(&format!("- {}\n", d));
            }
            out.push('\n');
        }

        if !self.files_modified.is_empty() {
            out.push_str("## Files Modified\n");
            for f in &self.files_modified {
                out.push_str(&format!("- {}\n", f));
            }
            out.push('\n');
        }

        if !self.errors_encountered.is_empty() {
            out.push_str("## Errors Encountered\n");
            for e in &self.errors_encountered {
                out.push_str(&format!("- {}\n", e));
            }
            out.push('\n');
        }

        if !self.general_summary.is_empty() {
            out.push_str(&format!("## Summary\n{}\n", self.general_summary));
        }

        out
    }
}

/// Get the file path for session memory storage.
pub fn session_memory_path(cwd: &Path, session_id: &str) -> PathBuf {
    cwd.join(".rho")
        .join("session-memory")
        .join(format!("{}.md", session_id))
}

/// Load session memory from the structured markdown file.
pub fn load_session_memory(cwd: &Path, session_id: &str) -> Option<SessionMemory> {
    let path = session_memory_path(cwd, session_id);
    let content = std::fs::read_to_string(&path).ok()?;
    Some(parse_session_memory(&content, session_id))
}

/// Save session memory to disk as structured markdown.
pub fn save_session_memory(cwd: &Path, memory: &SessionMemory) -> std::io::Result<()> {
    let path = session_memory_path(cwd, &memory.session_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut content = String::from("# Session Memory\n\n");

    content.push_str(&format!("## Current Task\n{}\n\n", memory.current_task));

    if !memory.key_decisions.is_empty() {
        content.push_str("## Key Decisions\n");
        for d in &memory.key_decisions {
            content.push_str(&format!("- {}\n", d));
        }
        content.push('\n');
    }

    if !memory.files_modified.is_empty() {
        content.push_str("## Files Modified\n");
        for f in &memory.files_modified {
            content.push_str(&format!("- {}\n", f));
        }
        content.push('\n');
    }

    if !memory.errors_encountered.is_empty() {
        content.push_str("## Errors Encountered\n");
        for e in &memory.errors_encountered {
            content.push_str(&format!("- {}\n", e));
        }
        content.push('\n');
    }

    if !memory.general_summary.is_empty() {
        content.push_str(&format!("## Summary\n{}\n", memory.general_summary));
    }

    std::fs::write(&path, content)
}

/// Parse session memory from structured markdown content.
fn parse_session_memory(content: &str, session_id: &str) -> SessionMemory {
    let mut memory = SessionMemory {
        session_id: session_id.to_string(),
        ..Default::default()
    };

    let mut current_section = "";

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("## ") {
            current_section = trimmed.strip_prefix("## ").unwrap_or("");
            continue;
        }

        if trimmed.is_empty() || trimmed.starts_with("# ") {
            continue;
        }

        let item = trimmed.strip_prefix("- ").unwrap_or(trimmed);

        match current_section {
            "Current Task" => {
                if memory.current_task.is_empty() {
                    memory.current_task = item.to_string();
                } else {
                    memory.current_task.push('\n');
                    memory.current_task.push_str(item);
                }
            }
            "Key Decisions" => memory.key_decisions.push(item.to_string()),
            "Files Modified" => memory.files_modified.push(item.to_string()),
            "Errors Encountered" => memory.errors_encountered.push(item.to_string()),
            "Summary" => {
                if memory.general_summary.is_empty() {
                    memory.general_summary = item.to_string();
                } else {
                    memory.general_summary.push('\n');
                    memory.general_summary.push_str(item);
                }
            }
            _ => {}
        }
    }

    memory
}

/// Build a prompt for the LLM to generate/update session memory from the conversation.
pub fn build_session_memory_prompt(
    existing_memory: Option<&SessionMemory>,
    conversation_summary: &str,
) -> String {
    let mut prompt = String::from(
        "You are maintaining a structured session memory. Analyze the conversation and produce \
         an updated summary. Output ONLY the following markdown sections, nothing else:\n\n\
         ## Current Task\n<what the user is currently working on>\n\n\
         ## Key Decisions\n- <bullet points of important decisions made>\n\n\
         ## Files Modified\n- <list of files that were changed>\n\n\
         ## Errors Encountered\n- <any errors or issues found>\n\n\
         ## Summary\n<brief overall summary of progress>\n\n",
    );

    if let Some(mem) = existing_memory {
        prompt.push_str("--- EXISTING MEMORY ---\n");
        prompt.push_str(&mem.format_as_message());
        prompt.push_str("\n--- END EXISTING MEMORY ---\n\n");
    }

    prompt.push_str("--- CONVERSATION ---\n");
    prompt.push_str(conversation_summary);
    prompt.push_str("\n--- END CONVERSATION ---\n");

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_session_memory() {
        let tmp = tempfile::tempdir().unwrap();
        let memory = SessionMemory {
            session_id: "test-123".into(),
            current_task: "Implement feature X".into(),
            key_decisions: vec!["Use approach A".into(), "Skip optimization".into()],
            files_modified: vec!["src/main.rs".into(), "src/lib.rs".into()],
            errors_encountered: vec!["compile error in foo()".into()],
            general_summary: "Making good progress on feature X".into(),
            tool_call_count_at_update: 10,
            token_estimate_at_update: 5000,
        };

        save_session_memory(tmp.path(), &memory).unwrap();
        let loaded = load_session_memory(tmp.path(), "test-123").unwrap();

        assert_eq!(loaded.current_task, "Implement feature X");
        assert_eq!(loaded.key_decisions.len(), 2);
        assert_eq!(loaded.key_decisions[0], "Use approach A");
        assert_eq!(loaded.files_modified.len(), 2);
        assert_eq!(loaded.errors_encountered.len(), 1);
        assert_eq!(loaded.general_summary, "Making good progress on feature X");
    }

    #[test]
    fn format_as_message_includes_sections() {
        let memory = SessionMemory {
            session_id: "s1".into(),
            current_task: "Fix bug".into(),
            key_decisions: vec!["Revert commit".into()],
            files_modified: vec![],
            errors_encountered: vec![],
            general_summary: "In progress".into(),
            ..Default::default()
        };

        let msg = memory.format_as_message();
        assert!(msg.contains("Fix bug"));
        assert!(msg.contains("Revert commit"));
        assert!(msg.contains("In progress"));
        assert!(!msg.contains("Files Modified")); // empty section omitted
    }

    #[test]
    fn load_nonexistent_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load_session_memory(tmp.path(), "nonexistent").is_none());
    }
}
