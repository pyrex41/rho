use serde::{Deserialize, Serialize};

// === Content Types ===

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Content {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "image")]
    Image { data: String, mime_type: String },
    #[serde(rename = "toolCall")]
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
}

// === Messages ===

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role")]
pub enum Message {
    #[serde(rename = "user")]
    User {
        content: UserContent,
        timestamp: u64,
    },
    #[serde(rename = "assistant")]
    Assistant {
        content: Vec<Content>,
        model: String,
        usage: Usage,
        stop_reason: StopReason,
        timestamp: u64,
    },
    #[serde(rename = "toolResult")]
    ToolResult {
        tool_call_id: String,
        tool_name: String,
        content: Vec<Content>,
        is_error: bool,
        timestamp: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    Text(String),
    Blocks(Vec<Content>),
}

// === Usage / Stop Reason ===

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StopReason {
    Stop,
    Length,
    ToolUse,
    Error,
    Aborted,
}

// === Model ===

#[derive(Debug, Clone)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub base_url: String,
    pub reasoning: bool,
    pub context_window: usize,
    pub max_tokens: usize,
    /// Optional xAI server-side tools to enable (e.g., "web_search", "x_search").
    /// Only used when provider is "xai". These are tools that run on xAI's servers
    /// and are automatically invoked by the model.
    pub xai_tools: Option<Vec<String>>,
}

// === Tool Definition ===

#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value, // JSON Schema
}

// === Agent Event (the union type that drives everything) ===

#[derive(Debug, Clone)]
pub enum AgentEvent {
    AgentStart,
    AgentEnd {
        messages: Vec<Message>,
    },
    TurnStart,
    TurnEnd {
        message: Message,
        tool_results: Vec<Message>,
    },
    MessageStart {
        message: Message,
    },
    MessageUpdate {
        message: Message,
        event: AssistantStreamEvent,
    },
    MessageEnd {
        message: Message,
    },
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: serde_json::Value,
    },
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        partial_result: ToolResult,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: ToolResult,
        is_error: bool,
    },
    ContextCompacted {
        original_estimate: usize,
        compacted_estimate: usize,
        messages_pruned: usize,
    },
}

// === Tool Result ===

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub content: Vec<Content>,
    pub details: serde_json::Value,
}

// === SSE Stream Events ===

#[derive(Debug, Clone)]
pub enum AssistantStreamEvent {
    Start,
    TextStart { index: usize },
    TextDelta { index: usize, delta: String },
    TextEnd { index: usize, content: String },
    ThinkingStart { index: usize },
    ThinkingDelta { index: usize, delta: String },
    ThinkingEnd { index: usize, content: String },
    ToolCallStart { index: usize },
    ToolCallDelta { index: usize, delta: String },
    ToolCallEnd { index: usize, tool_call: Content },
    Done { stop_reason: StopReason },
    Error { stop_reason: StopReason },
}

// === Thinking Level ===

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq)]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    #[default]
    Medium,
    High,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_text_serialization() {
        let content = Content::Text {
            text: "hello".into(),
        };
        let json = serde_json::to_string(&content).unwrap();
        assert!(json.contains(r#""type":"text""#));
        assert!(json.contains(r#""text":"hello""#));

        let roundtrip: Content = serde_json::from_str(&json).unwrap();
        match roundtrip {
            Content::Text { text } => assert_eq!(text, "hello"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn content_thinking_serialization() {
        let content = Content::Thinking {
            thinking: "reasoning here".into(),
        };
        let json = serde_json::to_string(&content).unwrap();
        assert!(json.contains(r#""type":"thinking""#));

        let roundtrip: Content = serde_json::from_str(&json).unwrap();
        match roundtrip {
            Content::Thinking { thinking } => assert_eq!(thinking, "reasoning here"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn content_image_serialization() {
        let content = Content::Image {
            data: "base64data".into(),
            mime_type: "image/png".into(),
        };
        let json = serde_json::to_string(&content).unwrap();
        assert!(json.contains(r#""type":"image""#));
        assert!(json.contains(r#""mime_type":"image/png""#));

        let roundtrip: Content = serde_json::from_str(&json).unwrap();
        match roundtrip {
            Content::Image { data, mime_type } => {
                assert_eq!(data, "base64data");
                assert_eq!(mime_type, "image/png");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn content_tool_call_serialization() {
        let content = Content::ToolCall {
            id: "call_123".into(),
            name: "Write".into(),
            arguments: serde_json::json!({"path": "/tmp/test.txt", "content": "hello"}),
        };
        let json = serde_json::to_string(&content).unwrap();
        assert!(json.contains(r#""type":"toolCall""#));
        assert!(json.contains(r#""name":"Write""#));

        let roundtrip: Content = serde_json::from_str(&json).unwrap();
        match roundtrip {
            Content::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id, "call_123");
                assert_eq!(name, "Write");
                assert_eq!(arguments["path"], "/tmp/test.txt");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn message_user_text_serialization() {
        let msg = Message::User {
            content: UserContent::Text("hello".into()),
            timestamp: 1234567890,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""role":"user""#));

        let roundtrip: Message = serde_json::from_str(&json).unwrap();
        match roundtrip {
            Message::User { content, timestamp } => {
                assert_eq!(timestamp, 1234567890);
                match content {
                    UserContent::Text(t) => assert_eq!(t, "hello"),
                    _ => panic!("wrong user content variant"),
                }
            }
            _ => panic!("wrong message variant"),
        }
    }

    #[test]
    fn message_user_blocks_serialization() {
        let msg = Message::User {
            content: UserContent::Blocks(vec![
                Content::Text {
                    text: "hello".into(),
                },
                Content::Image {
                    data: "abc".into(),
                    mime_type: "image/jpeg".into(),
                },
            ]),
            timestamp: 100,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let roundtrip: Message = serde_json::from_str(&json).unwrap();
        match roundtrip {
            Message::User { content, .. } => match content {
                UserContent::Blocks(blocks) => assert_eq!(blocks.len(), 2),
                _ => panic!("wrong user content variant"),
            },
            _ => panic!("wrong message variant"),
        }
    }

    #[test]
    fn message_assistant_serialization() {
        let msg = Message::Assistant {
            content: vec![Content::Text {
                text: "response".into(),
            }],
            model: "claude-sonnet-4-5-20250929".into(),
            usage: Usage {
                input: 100,
                output: 50,
                cache_read: 10,
                cache_write: 5,
            },
            stop_reason: StopReason::Stop,
            timestamp: 1234567890,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""role":"assistant""#));
        assert!(json.contains(r#""stop_reason":"Stop""#));

        let roundtrip: Message = serde_json::from_str(&json).unwrap();
        match roundtrip {
            Message::Assistant {
                content,
                model,
                usage,
                stop_reason,
                ..
            } => {
                assert_eq!(content.len(), 1);
                assert_eq!(model, "claude-sonnet-4-5-20250929");
                assert_eq!(usage.input, 100);
                assert_eq!(stop_reason, StopReason::Stop);
            }
            _ => panic!("wrong message variant"),
        }
    }

    #[test]
    fn message_tool_result_serialization() {
        let msg = Message::ToolResult {
            tool_call_id: "call_123".into(),
            tool_name: "Write".into(),
            content: vec![Content::Text {
                text: "wrote 5 bytes".into(),
            }],
            is_error: false,
            timestamp: 100,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""role":"toolResult""#));
        assert!(json.contains(r#""tool_call_id":"call_123""#));

        let roundtrip: Message = serde_json::from_str(&json).unwrap();
        match roundtrip {
            Message::ToolResult {
                tool_call_id,
                tool_name,
                is_error,
                ..
            } => {
                assert_eq!(tool_call_id, "call_123");
                assert_eq!(tool_name, "Write");
                assert!(!is_error);
            }
            _ => panic!("wrong message variant"),
        }
    }

    #[test]
    fn usage_default() {
        let usage = Usage::default();
        assert_eq!(usage.input, 0);
        assert_eq!(usage.output, 0);
        assert_eq!(usage.cache_read, 0);
        assert_eq!(usage.cache_write, 0);
    }

    #[test]
    fn stop_reason_serialization() {
        let reasons = vec![
            (StopReason::Stop, r#""Stop""#),
            (StopReason::Length, r#""Length""#),
            (StopReason::ToolUse, r#""ToolUse""#),
            (StopReason::Error, r#""Error""#),
            (StopReason::Aborted, r#""Aborted""#),
        ];
        for (reason, expected_json) in reasons {
            let json = serde_json::to_string(&reason).unwrap();
            assert_eq!(json, expected_json);
            let roundtrip: StopReason = serde_json::from_str(&json).unwrap();
            assert_eq!(roundtrip, reason);
        }
    }

    #[test]
    fn thinking_level_default() {
        let level = ThinkingLevel::default();
        assert_eq!(level, ThinkingLevel::Medium);
    }

    #[test]
    fn thinking_level_serialization() {
        let levels = vec![
            ThinkingLevel::Off,
            ThinkingLevel::Minimal,
            ThinkingLevel::Low,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
        ];
        for level in levels {
            let json = serde_json::to_string(&level).unwrap();
            let roundtrip: ThinkingLevel = serde_json::from_str(&json).unwrap();
            assert_eq!(roundtrip, level);
        }
    }
}
