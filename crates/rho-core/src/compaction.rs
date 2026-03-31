use crate::types::{Content, Message, Model, UserContent};

/// Estimate token count using chars/4 heuristic.
pub fn estimate_tokens(messages: &[Message]) -> usize {
    let mut chars = 0;
    for msg in messages {
        match msg {
            Message::User { content, .. } => match content {
                UserContent::Text(t) => chars += t.len(),
                UserContent::Blocks(blocks) => {
                    for block in blocks {
                        chars += content_chars(block);
                    }
                }
            },
            Message::Assistant { content, .. } => {
                for block in content {
                    chars += content_chars(block);
                }
            }
            Message::ToolResult { content, .. } => {
                for block in content {
                    chars += content_chars(block);
                }
            }
        }
    }
    chars / 4
}

fn content_chars(c: &Content) -> usize {
    match c {
        Content::Text { text } => text.len(),
        Content::Thinking { thinking } => thinking.len(),
        Content::ToolCall { arguments, .. } => arguments.to_string().len() + 50, // overhead
        Content::Image { data, .. } => data.len(),
    }
}

/// Replace old tool result content with "[pruned]".
/// Keeps the last `keep_recent` messages intact.
pub fn prune_tool_outputs(messages: &[Message], keep_recent: usize) -> Vec<Message> {
    let len = messages.len();
    let prune_boundary = len.saturating_sub(keep_recent);

    messages
        .iter()
        .enumerate()
        .map(|(i, msg)| {
            if i < prune_boundary {
                match msg {
                    Message::ToolResult {
                        tool_call_id,
                        tool_name,
                        is_error,
                        timestamp,
                        ..
                    } => Message::ToolResult {
                        tool_call_id: tool_call_id.clone(),
                        tool_name: tool_name.clone(),
                        content: vec![Content::Text {
                            text: "[pruned]".into(),
                        }],
                        is_error: *is_error,
                        timestamp: *timestamp,
                    },
                    other => other.clone(),
                }
            } else {
                msg.clone()
            }
        })
        .collect()
}

/// Build the transform function for AgentLoopConfig.
///
/// Three-tier auto-compaction strategy:
/// 1. Tier 1 (MicroCompact): Prune old tool outputs (no LLM call)
/// 2. Tier 2 (SessionMemory): Replace old messages with session memory summary
/// 3. If neither suffices, aggressive pruning
pub fn make_compaction_transform(
    threshold: f64,
    session_memory_path: Option<std::path::PathBuf>,
) -> Box<dyn Fn(&[Message], &Model) -> (Vec<Message>, Option<CompactionResult>) + Send + Sync> {
    Box::new(move |messages: &[Message], model: &Model| {
        let estimated = estimate_tokens(messages);
        let limit = (threshold * model.context_window as f64) as usize;

        if estimated <= limit {
            return (messages.to_vec(), None);
        }

        // Tier 1a: Prune tool outputs, keep last 6
        let pruned = prune_tool_outputs(messages, 6);
        let new_estimate = estimate_tokens(&pruned);

        if new_estimate <= limit {
            return (
                pruned,
                Some(CompactionResult {
                    original_estimate: estimated,
                    compacted_estimate: new_estimate,
                    messages_pruned: messages.len().saturating_sub(6),
                    tier: CompactionTier::MicroCompact,
                }),
            );
        }

        // Tier 1b: More aggressive pruning — keep last 2
        let pruned = prune_tool_outputs(messages, 2);
        let new_estimate = estimate_tokens(&pruned);

        if new_estimate <= limit {
            return (
                pruned,
                Some(CompactionResult {
                    original_estimate: estimated,
                    compacted_estimate: new_estimate,
                    messages_pruned: messages.len().saturating_sub(2),
                    tier: CompactionTier::MicroCompact,
                }),
            );
        }

        // Tier 2: Session memory compaction — replace old prefix with summary
        if let Some(ref mem_path) = session_memory_path {
            if let Ok(content) = std::fs::read_to_string(mem_path) {
                if !content.trim().is_empty() {
                    let keep_recent = 6.min(messages.len());
                    let summary_msg = Message::User {
                        content: UserContent::Text(format!(
                            "[Context compacted — session memory follows]\n\n{}",
                            content
                        )),
                        timestamp: 0,
                    };
                    let mut compacted = vec![summary_msg];
                    compacted.extend_from_slice(&messages[messages.len() - keep_recent..]);
                    let new_estimate = estimate_tokens(&compacted);

                    return (
                        compacted,
                        Some(CompactionResult {
                            original_estimate: estimated,
                            compacted_estimate: new_estimate,
                            messages_pruned: messages.len() - keep_recent,
                            tier: CompactionTier::SessionMemory,
                        }),
                    );
                }
            }
        }

        // Fallback: return aggressive prune result
        (
            pruned,
            Some(CompactionResult {
                original_estimate: estimated,
                compacted_estimate: new_estimate,
                messages_pruned: messages.len().saturating_sub(2),
                tier: CompactionTier::MicroCompact,
            }),
        )
    })
}

#[derive(Debug, Clone)]
pub struct CompactionResult {
    pub original_estimate: usize,
    pub compacted_estimate: usize,
    pub messages_pruned: usize,
    pub tier: CompactionTier,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CompactionTier {
    /// Tier 1: Prune old tool outputs (no LLM call).
    MicroCompact,
    /// Tier 2: Replace old messages with session memory summary.
    SessionMemory,
}

/// Build a summary prompt for manual /compact.
pub fn build_summary_prompt(messages: &[Message]) -> String {
    let mut context = String::new();
    for msg in messages {
        match msg {
            Message::User { content, .. } => {
                context.push_str("User: ");
                match content {
                    UserContent::Text(t) => context.push_str(t),
                    UserContent::Blocks(blocks) => {
                        for b in blocks {
                            if let Content::Text { text } = b {
                                context.push_str(text);
                            }
                        }
                    }
                }
                context.push('\n');
            }
            Message::Assistant { content, .. } => {
                context.push_str("Assistant: ");
                for b in content {
                    if let Content::Text { text } = b {
                        context.push_str(text);
                    }
                }
                context.push('\n');
            }
            Message::ToolResult {
                tool_name, content, ..
            } => {
                context.push_str(&format!("[Tool: {}] ", tool_name));
                for b in content {
                    if let Content::Text { text } = b {
                        // Truncate long tool results in summary
                        if text.len() > 500 {
                            context.push_str(&text[..500]);
                            context.push_str("...");
                        } else {
                            context.push_str(text);
                        }
                    }
                }
                context.push('\n');
            }
        }
    }

    format!(
        "Summarize the following conversation concisely, preserving key decisions, \
         file paths, code changes, and important context. Be brief but complete.\n\n\
         ---\n{}\n---\n\nProvide a concise summary:",
        context
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Usage;

    fn make_user(text: &str) -> Message {
        Message::User {
            content: UserContent::Text(text.into()),
            timestamp: 0,
        }
    }

    fn make_assistant(text: &str) -> Message {
        Message::Assistant {
            content: vec![Content::Text { text: text.into() }],
            model: "test".into(),
            usage: Usage::default(),
            stop_reason: crate::types::StopReason::Stop,
            timestamp: 0,
        }
    }

    fn make_tool_result(name: &str, text: &str) -> Message {
        Message::ToolResult {
            tool_call_id: "id".into(),
            tool_name: name.into(),
            content: vec![Content::Text { text: text.into() }],
            is_error: false,
            timestamp: 0,
        }
    }

    #[test]
    fn estimate_tokens_basic() {
        let msgs = vec![
            make_user("hello world"), // 11 chars
        ];
        let est = estimate_tokens(&msgs);
        assert_eq!(est, 2); // 11/4 = 2
    }

    #[test]
    fn prune_tool_outputs_keeps_recent() {
        let msgs = vec![
            make_user("q1"),
            make_tool_result("read", "long content here"),
            make_assistant("a1"),
            make_user("q2"),
            make_tool_result("read", "more content"),
            make_assistant("a2"),
        ];
        let pruned = prune_tool_outputs(&msgs, 3);

        // First 3 messages are in prune zone
        match &pruned[1] {
            Message::ToolResult { content, .. } => {
                assert_eq!(content[0], Content::Text { text: "[pruned]".into() });
            }
            _ => panic!("expected tool result"),
        }

        // Last 3 messages kept intact
        match &pruned[4] {
            Message::ToolResult { content, .. } => {
                match &content[0] {
                    Content::Text { text } => assert_eq!(text, "more content"),
                    _ => panic!("expected text"),
                }
            }
            _ => panic!("expected tool result"),
        }
    }

    #[test]
    fn compaction_transform_no_op_under_threshold() {
        let transform = make_compaction_transform(0.8, None);
        let model = Model {
            id: "test".into(),
            name: "test".into(),
            provider: "test".into(),
            base_url: String::new(),
            reasoning: false,
            context_window: 200_000,
            max_tokens: 8192,
        };
        let msgs = vec![make_user("short")];
        let (result, compaction) = transform(&msgs, &model);
        assert_eq!(result.len(), 1);
        assert!(compaction.is_none());
    }

    #[test]
    fn build_summary_prompt_includes_messages() {
        let msgs = vec![
            make_user("Find auth module"),
            make_assistant("I found it in src/auth.rs"),
        ];
        let prompt = build_summary_prompt(&msgs);
        assert!(prompt.contains("Find auth module"));
        assert!(prompt.contains("src/auth.rs"));
    }
}
