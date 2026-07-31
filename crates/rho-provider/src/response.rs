use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use rho_core::types::{AssistantStreamEvent, Content, Message, StopReason, Usage};

use crate::sse::SseEvent;

#[derive(Debug, Clone, Copy, PartialEq)]
enum BlockType {
    Text,
    Thinking,
    ToolUse,
}

pub struct ResponseHandler {
    block_types: Vec<BlockType>,
    tool_json_accumulators: HashMap<usize, String>,
    tool_metadata: HashMap<usize, (String, String)>, // (id, name)
    text_accumulators: HashMap<usize, String>,
    usage: Usage,
    model: String,
    stop_reason: StopReason,
}

impl Default for ResponseHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl ResponseHandler {
    pub fn new() -> Self {
        Self {
            block_types: Vec::new(),
            tool_json_accumulators: HashMap::new(),
            tool_metadata: HashMap::new(),
            text_accumulators: HashMap::new(),
            usage: Usage::default(),
            model: String::new(),
            stop_reason: StopReason::Stop,
        }
    }

    pub fn handle_event(&mut self, event: &SseEvent) -> Vec<AssistantStreamEvent> {
        match event.event_type.as_str() {
            "message_start" => self.handle_message_start(&event.data),
            "content_block_start" => self.handle_content_block_start(&event.data),
            "content_block_delta" => self.handle_content_block_delta(&event.data),
            "content_block_stop" => self.handle_content_block_stop(&event.data),
            "message_delta" => self.handle_message_delta(&event.data),
            "message_stop" | "ping" => vec![],
            "error" => vec![AssistantStreamEvent::Error {
                stop_reason: StopReason::Error,
            }],
            _ => vec![],
        }
    }

    fn handle_message_start(&mut self, data: &str) -> Vec<AssistantStreamEvent> {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(data) {
            if let Some(message) = parsed.get("message") {
                self.model = message
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(usage) = message.get("usage") {
                    self.usage = parse_usage(usage);
                }
            }
        }
        vec![AssistantStreamEvent::Start]
    }

    fn handle_content_block_start(&mut self, data: &str) -> Vec<AssistantStreamEvent> {
        let parsed: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return vec![],
        };

        let index = parsed.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let block_type_str = parsed
            .get("content_block")
            .and_then(|b| b.get("type"))
            .and_then(|v| v.as_str())
            .unwrap_or("text");

        let block_type = match block_type_str {
            "thinking" => BlockType::Thinking,
            "tool_use" => {
                // Extract tool id and name
                if let Some(block) = parsed.get("content_block") {
                    let id = block
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = block
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    self.tool_metadata.insert(index, (id, name));
                    self.tool_json_accumulators.insert(index, String::new());
                }
                BlockType::ToolUse
            }
            _ => BlockType::Text,
        };

        // Ensure block_types vec is large enough
        while self.block_types.len() <= index {
            self.block_types.push(BlockType::Text);
        }
        self.block_types[index] = block_type;
        self.text_accumulators.insert(index, String::new());

        match block_type {
            BlockType::Text => vec![AssistantStreamEvent::TextStart { index }],
            BlockType::Thinking => vec![AssistantStreamEvent::ThinkingStart { index }],
            BlockType::ToolUse => vec![AssistantStreamEvent::ToolCallStart { index }],
        }
    }

    fn handle_content_block_delta(&mut self, data: &str) -> Vec<AssistantStreamEvent> {
        let parsed: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return vec![],
        };

        let index = parsed.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

        let delta = match parsed.get("delta") {
            Some(d) => d,
            None => return vec![],
        };

        let delta_type = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match delta_type {
            "text_delta" => {
                let text = delta
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                self.text_accumulators
                    .entry(index)
                    .or_default()
                    .push_str(&text);
                vec![AssistantStreamEvent::TextDelta { index, delta: text }]
            }
            "thinking_delta" => {
                let thinking = delta
                    .get("thinking")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                self.text_accumulators
                    .entry(index)
                    .or_default()
                    .push_str(&thinking);
                vec![AssistantStreamEvent::ThinkingDelta {
                    index,
                    delta: thinking,
                }]
            }
            "input_json_delta" => {
                let partial = delta
                    .get("partial_json")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                self.tool_json_accumulators
                    .entry(index)
                    .or_default()
                    .push_str(&partial);
                vec![AssistantStreamEvent::ToolCallDelta {
                    index,
                    delta: partial,
                }]
            }
            _ => vec![],
        }
    }

    fn handle_content_block_stop(&mut self, data: &str) -> Vec<AssistantStreamEvent> {
        let parsed: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return vec![],
        };

        let index = parsed.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

        let block_type = self
            .block_types
            .get(index)
            .copied()
            .unwrap_or(BlockType::Text);

        match block_type {
            BlockType::Text => {
                let content = self
                    .text_accumulators
                    .get(&index)
                    .cloned()
                    .unwrap_or_default();
                vec![AssistantStreamEvent::TextEnd { index, content }]
            }
            BlockType::Thinking => {
                let content = self
                    .text_accumulators
                    .get(&index)
                    .cloned()
                    .unwrap_or_default();
                vec![AssistantStreamEvent::ThinkingEnd { index, content }]
            }
            BlockType::ToolUse => {
                let json_str = self
                    .tool_json_accumulators
                    .get(&index)
                    .cloned()
                    .unwrap_or_default();
                let (id, name) = self.tool_metadata.get(&index).cloned().unwrap_or_default();

                let arguments: serde_json::Value = serde_json::from_str(&json_str)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

                vec![AssistantStreamEvent::ToolCallEnd {
                    index,
                    tool_call: Content::ToolCall {
                        id,
                        name,
                        arguments,
                    },
                }]
            }
        }
    }

    fn handle_message_delta(&mut self, data: &str) -> Vec<AssistantStreamEvent> {
        let parsed: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return vec![],
        };

        // Update usage from message delta if present
        if let Some(usage) = parsed.get("usage") {
            let delta_usage = parse_usage(usage);
            // message_delta typically has output_tokens
            if delta_usage.output > 0 {
                self.usage.output = delta_usage.output;
            }
        }

        let stop_reason_str = parsed
            .get("delta")
            .and_then(|d| d.get("stop_reason"))
            .and_then(|v| v.as_str())
            .unwrap_or("end_turn");

        let stop_reason = match stop_reason_str {
            "end_turn" => StopReason::Stop,
            "max_tokens" => StopReason::Length,
            "tool_use" => StopReason::ToolUse,
            _ => StopReason::Stop,
        };

        self.stop_reason = stop_reason.clone();

        vec![AssistantStreamEvent::Done { stop_reason }]
    }

    pub fn build_final_message(&self) -> Message {
        let mut content_blocks = Vec::new();

        for (index, block_type) in self.block_types.iter().enumerate() {
            match block_type {
                BlockType::Text => {
                    let text = self
                        .text_accumulators
                        .get(&index)
                        .cloned()
                        .unwrap_or_default();
                    content_blocks.push(Content::Text { text });
                }
                BlockType::Thinking => {
                    let thinking = self
                        .text_accumulators
                        .get(&index)
                        .cloned()
                        .unwrap_or_default();
                    content_blocks.push(Content::Thinking { thinking });
                }
                BlockType::ToolUse => {
                    let json_str = self
                        .tool_json_accumulators
                        .get(&index)
                        .cloned()
                        .unwrap_or_default();
                    let (id, name) = self.tool_metadata.get(&index).cloned().unwrap_or_default();
                    let arguments: serde_json::Value = serde_json::from_str(&json_str)
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                    content_blocks.push(Content::ToolCall {
                        id,
                        name,
                        arguments,
                    });
                }
            }
        }

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Message::Assistant {
            content: content_blocks,
            model: self.model.clone(),
            usage: self.usage.clone(),
            stop_reason: self.stop_reason.clone(),
            timestamp,
        }
    }
}

fn parse_usage(value: &serde_json::Value) -> Usage {
    Usage {
        input: value
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        output: value
            .get("output_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        cache_read: value
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        cache_write: value
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sse(event_type: &str, data: &str) -> SseEvent {
        SseEvent {
            event_type: event_type.to_string(),
            data: data.to_string(),
        }
    }

    #[test]
    fn test_message_start() {
        let mut handler = ResponseHandler::new();
        let events = handler.handle_event(&sse(
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_01","type":"message","role":"assistant","model":"claude-sonnet-4-5-20250929","content":[],"usage":{"input_tokens":100,"output_tokens":0}}}"#,
        ));

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], AssistantStreamEvent::Start));
        assert_eq!(handler.model, "claude-sonnet-4-5-20250929");
        assert_eq!(handler.usage.input, 100);
    }

    #[test]
    fn test_text_content_block_flow() {
        let mut handler = ResponseHandler::new();

        // Start
        handler.handle_event(&sse(
            "message_start",
            r#"{"message":{"model":"claude","usage":{"input_tokens":10,"output_tokens":0}}}"#,
        ));

        // Content block start
        let events = handler.handle_event(&sse(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        ));
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            AssistantStreamEvent::TextStart { index: 0 }
        ));

        // Deltas
        let events = handler.handle_event(&sse(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
        ));
        assert_eq!(events.len(), 1);
        match &events[0] {
            AssistantStreamEvent::TextDelta { index, delta } => {
                assert_eq!(*index, 0);
                assert_eq!(delta, "Hello");
            }
            _ => panic!("expected TextDelta"),
        }

        let _ = handler.handle_event(&sse(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" world"}}"#,
        ));

        // Content block stop
        let events = handler.handle_event(&sse(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ));
        assert_eq!(events.len(), 1);
        match &events[0] {
            AssistantStreamEvent::TextEnd { index, content } => {
                assert_eq!(*index, 0);
                assert_eq!(content, "Hello world");
            }
            _ => panic!("expected TextEnd"),
        }
    }

    #[test]
    fn test_tool_call_flow() {
        let mut handler = ResponseHandler::new();

        handler.handle_event(&sse(
            "message_start",
            r#"{"message":{"model":"claude","usage":{"input_tokens":10,"output_tokens":0}}}"#,
        ));

        // Tool use content block start
        let events = handler.handle_event(&sse(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_01","name":"read_file"}}"#,
        ));
        assert!(matches!(
            events[0],
            AssistantStreamEvent::ToolCallStart { index: 0 }
        ));

        // JSON deltas
        handler.handle_event(&sse(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#,
        ));
        handler.handle_event(&sse(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"/tmp/test\"}"}}"#,
        ));

        // Stop
        let events = handler.handle_event(&sse(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ));
        assert_eq!(events.len(), 1);
        match &events[0] {
            AssistantStreamEvent::ToolCallEnd { index, tool_call } => {
                assert_eq!(*index, 0);
                match tool_call {
                    Content::ToolCall {
                        id,
                        name,
                        arguments,
                    } => {
                        assert_eq!(id, "toolu_01");
                        assert_eq!(name, "read_file");
                        assert_eq!(arguments["path"], "/tmp/test");
                    }
                    _ => panic!("expected ToolCall content"),
                }
            }
            _ => panic!("expected ToolCallEnd"),
        }
    }

    #[test]
    fn test_thinking_flow() {
        let mut handler = ResponseHandler::new();

        handler.handle_event(&sse(
            "message_start",
            r#"{"message":{"model":"claude","usage":{"input_tokens":10,"output_tokens":0}}}"#,
        ));

        let events = handler.handle_event(&sse(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
        ));
        assert!(matches!(
            events[0],
            AssistantStreamEvent::ThinkingStart { index: 0 }
        ));

        handler.handle_event(&sse(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me think"}}"#,
        ));

        let events = handler.handle_event(&sse(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ));
        match &events[0] {
            AssistantStreamEvent::ThinkingEnd { index, content } => {
                assert_eq!(*index, 0);
                assert_eq!(content, "Let me think");
            }
            _ => panic!("expected ThinkingEnd"),
        }
    }

    #[test]
    fn test_message_delta_stop_reasons() {
        let mut handler = ResponseHandler::new();

        let events = handler.handle_event(&sse(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":50}}"#,
        ));
        match &events[0] {
            AssistantStreamEvent::Done { stop_reason } => {
                assert_eq!(*stop_reason, StopReason::Stop);
            }
            _ => panic!("expected Done"),
        }

        let events = handler.handle_event(&sse(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":30}}"#,
        ));
        match &events[0] {
            AssistantStreamEvent::Done { stop_reason } => {
                assert_eq!(*stop_reason, StopReason::ToolUse);
            }
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn test_build_final_message() {
        let mut handler = ResponseHandler::new();

        handler.handle_event(&sse(
            "message_start",
            r#"{"message":{"model":"claude-sonnet-4-5-20250929","usage":{"input_tokens":100,"output_tokens":0}}}"#,
        ));
        handler.handle_event(&sse(
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"text","text":""}}"#,
        ));
        handler.handle_event(&sse(
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"text_delta","text":"Hello!"}}"#,
        ));
        handler.handle_event(&sse("content_block_stop", r#"{"index":0}"#));
        handler.handle_event(&sse(
            "message_delta",
            r#"{"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":10}}"#,
        ));

        let msg = handler.build_final_message();
        match msg {
            Message::Assistant {
                content,
                model,
                usage,
                stop_reason,
                ..
            } => {
                assert_eq!(model, "claude-sonnet-4-5-20250929");
                assert_eq!(usage.input, 100);
                assert_eq!(usage.output, 10);
                assert_eq!(stop_reason, StopReason::Stop);
                assert_eq!(content.len(), 1);
                match &content[0] {
                    Content::Text { text } => assert_eq!(text, "Hello!"),
                    _ => panic!("expected Text content"),
                }
            }
            _ => panic!("expected Assistant message"),
        }
    }

    #[test]
    fn test_ping_and_message_stop_ignored() {
        let mut handler = ResponseHandler::new();
        assert!(handler.handle_event(&sse("ping", "{}")).is_empty());
        assert!(handler.handle_event(&sse("message_stop", "{}")).is_empty());
    }

    #[test]
    fn test_error_event() {
        let mut handler = ResponseHandler::new();
        let events = handler.handle_event(&sse("error", r#"{"type":"error"}"#));
        assert_eq!(events.len(), 1);
        match &events[0] {
            AssistantStreamEvent::Error { stop_reason } => {
                assert_eq!(*stop_reason, StopReason::Error);
            }
            _ => panic!("expected Error"),
        }
    }
}
