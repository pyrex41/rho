use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use rho_core::types::{AssistantStreamEvent, Content, Message, StopReason, Usage};

use crate::sse::SseEvent;

/// Tracks an in-progress tool call being accumulated from streaming chunks.
#[derive(Debug, Default)]
struct ToolCallAccumulator {
    id: String,
    name: String,
    arguments: String,
}

/// Handles OpenAI-compatible SSE streaming chunks from xAI's chat completions API.
///
/// The OpenAI streaming format differs from Anthropic's:
/// - No `event:` field in SSE; all data comes as `data:` lines
/// - Each chunk is a `chat.completion.chunk` JSON object
/// - Text content arrives via `delta.content`
/// - Tool calls arrive via `delta.tool_calls` array
/// - Stream ends with `data: [DONE]`
/// - Usage comes in the final chunk (with `stream_options.include_usage`)
pub struct ResponseHandler {
    model: String,
    text_accumulator: String,
    tool_calls: HashMap<usize, ToolCallAccumulator>,
    usage: Usage,
    stop_reason: StopReason,
    has_text_started: bool,
    has_emitted_start: bool,
    // Track tool call start events so we only emit once per tool call
    tool_starts_emitted: HashMap<usize, bool>,
}

impl Default for ResponseHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl ResponseHandler {
    pub fn new() -> Self {
        Self {
            model: String::new(),
            text_accumulator: String::new(),
            tool_calls: HashMap::new(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            has_text_started: false,
            has_emitted_start: false,
            tool_starts_emitted: HashMap::new(),
        }
    }

    pub fn handle_event(&mut self, event: &SseEvent) -> Vec<AssistantStreamEvent> {
        let data = event.data.trim();

        // OpenAI uses `data: [DONE]` to signal end of stream
        if data == "[DONE]" {
            return self.handle_done();
        }

        // Parse JSON chunk
        let chunk: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return vec![],
        };

        let mut events = Vec::new();

        // Emit Start event on first chunk
        if !self.has_emitted_start {
            self.has_emitted_start = true;

            // Extract model from first chunk
            if let Some(model) = chunk.get("model").and_then(|v| v.as_str()) {
                self.model = model.to_string();
            }

            events.push(AssistantStreamEvent::Start);
        }

        // Process choices
        if let Some(choices) = chunk.get("choices").and_then(|v| v.as_array()) {
            for choice in choices {
                let delta = choice.get("delta");
                let finish_reason = choice
                    .get("finish_reason")
                    .and_then(|v| v.as_str());

                if let Some(delta) = delta {
                    // Handle text content
                    if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                        if !content.is_empty() {
                            if !self.has_text_started {
                                self.has_text_started = true;
                                events.push(AssistantStreamEvent::TextStart { index: 0 });
                            }
                            self.text_accumulator.push_str(content);
                            events.push(AssistantStreamEvent::TextDelta {
                                index: 0,
                                delta: content.to_string(),
                            });
                        }
                    }

                    // Handle tool calls
                    if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                        for tc in tool_calls {
                            let tc_index = tc
                                .get("index")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0) as usize;

                            let accumulator = self
                                .tool_calls
                                .entry(tc_index)
                                .or_default();

                            // Extract id and name (usually in the first chunk for this tool call)
                            if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                                accumulator.id = id.to_string();
                            }
                            if let Some(function) = tc.get("function") {
                                if let Some(name) = function.get("name").and_then(|v| v.as_str()) {
                                    accumulator.name = name.to_string();
                                }
                                if let Some(args) =
                                    function.get("arguments").and_then(|v| v.as_str())
                                {
                                    accumulator.arguments.push_str(args);
                                }
                            }

                            // Emit ToolCallStart on first encounter
                            if !self.tool_starts_emitted.contains_key(&tc_index) {
                                self.tool_starts_emitted.insert(tc_index, true);
                                // Use index offset by 1 if we have text (text is at index 0)
                                let event_index = if self.has_text_started {
                                    tc_index + 1
                                } else {
                                    tc_index
                                };
                                events.push(AssistantStreamEvent::ToolCallStart {
                                    index: event_index,
                                });
                            }

                            // Emit tool call delta for argument streaming
                            if let Some(function) = tc.get("function") {
                                if let Some(args) =
                                    function.get("arguments").and_then(|v| v.as_str())
                                {
                                    if !args.is_empty() {
                                        let event_index = if self.has_text_started {
                                            tc_index + 1
                                        } else {
                                            tc_index
                                        };
                                        events.push(AssistantStreamEvent::ToolCallDelta {
                                            index: event_index,
                                            delta: args.to_string(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }

                // Handle finish_reason
                if let Some(reason) = finish_reason {
                    self.stop_reason = match reason {
                        "stop" => StopReason::Stop,
                        "tool_calls" => StopReason::ToolUse,
                        "length" => StopReason::Length,
                        _ => StopReason::Stop,
                    };
                }
            }
        }

        // Extract usage if present (comes in final chunk with include_usage)
        if let Some(usage) = chunk.get("usage") {
            self.usage = parse_usage(usage);
        }

        events
    }

    /// Called when `[DONE]` is received. Emits all end events.
    fn handle_done(&mut self) -> Vec<AssistantStreamEvent> {
        let mut events = Vec::new();

        // Close text block if open
        if self.has_text_started {
            events.push(AssistantStreamEvent::TextEnd {
                index: 0,
                content: self.text_accumulator.clone(),
            });
        }

        // Close all tool calls
        let mut sorted_indices: Vec<usize> = self.tool_calls.keys().copied().collect();
        sorted_indices.sort();

        for tc_index in sorted_indices {
            if let Some(tc) = self.tool_calls.get(&tc_index) {
                let arguments: serde_json::Value =
                    serde_json::from_str(&tc.arguments).unwrap_or(serde_json::Value::Object(
                        serde_json::Map::new(),
                    ));

                let event_index = if self.has_text_started {
                    tc_index + 1
                } else {
                    tc_index
                };

                events.push(AssistantStreamEvent::ToolCallEnd {
                    index: event_index,
                    tool_call: Content::ToolCall {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        arguments,
                    },
                });
            }
        }

        events.push(AssistantStreamEvent::Done {
            stop_reason: self.stop_reason.clone(),
        });

        events
    }

    pub fn build_final_message(&self) -> Message {
        let mut content_blocks = Vec::new();

        // Add text block if present
        if self.has_text_started {
            content_blocks.push(Content::Text {
                text: self.text_accumulator.clone(),
            });
        }

        // Add tool calls in order
        let mut sorted_indices: Vec<usize> = self.tool_calls.keys().copied().collect();
        sorted_indices.sort();

        for tc_index in sorted_indices {
            if let Some(tc) = self.tool_calls.get(&tc_index) {
                let arguments: serde_json::Value =
                    serde_json::from_str(&tc.arguments).unwrap_or(serde_json::Value::Object(
                        serde_json::Map::new(),
                    ));
                content_blocks.push(Content::ToolCall {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    arguments,
                });
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
            .get("prompt_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        output: value
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        cache_read: 0,
        cache_write: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sse(data: &str) -> SseEvent {
        SseEvent {
            event_type: String::new(),
            data: data.to_string(),
        }
    }

    #[test]
    fn test_text_streaming() {
        let mut handler = ResponseHandler::new();

        // First chunk with role
        let events = handler.handle_event(&sse(
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1234,"model":"grok-3","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}"#,
        ));
        assert_eq!(events.len(), 1); // Start
        assert!(matches!(events[0], AssistantStreamEvent::Start));

        // Text delta
        let events = handler.handle_event(&sse(
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1234,"model":"grok-3","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#,
        ));
        assert_eq!(events.len(), 2); // TextStart + TextDelta
        assert!(matches!(events[0], AssistantStreamEvent::TextStart { index: 0 }));
        match &events[1] {
            AssistantStreamEvent::TextDelta { index, delta } => {
                assert_eq!(*index, 0);
                assert_eq!(delta, "Hello");
            }
            _ => panic!("expected TextDelta"),
        }

        // More text
        let events = handler.handle_event(&sse(
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1234,"model":"grok-3","choices":[{"index":0,"delta":{"content":" world"},"finish_reason":null}]}"#,
        ));
        assert_eq!(events.len(), 1); // Just TextDelta
        match &events[0] {
            AssistantStreamEvent::TextDelta { delta, .. } => assert_eq!(delta, " world"),
            _ => panic!("expected TextDelta"),
        }

        // Finish
        let events = handler.handle_event(&sse(
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1234,"model":"grok-3","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#,
        ));
        assert_eq!(events.len(), 0); // finish_reason doesn't emit events directly

        // [DONE]
        let events = handler.handle_event(&sse("[DONE]"));
        assert!(events.len() >= 2); // TextEnd + Done
        match &events[0] {
            AssistantStreamEvent::TextEnd { content, .. } => {
                assert_eq!(content, "Hello world");
            }
            _ => panic!("expected TextEnd"),
        }
        assert!(matches!(
            events.last().unwrap(),
            AssistantStreamEvent::Done {
                stop_reason: StopReason::Stop
            }
        ));
    }

    #[test]
    fn test_tool_call_streaming() {
        let mut handler = ResponseHandler::new();

        // Start
        handler.handle_event(&sse(
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1234,"model":"grok-3","choices":[{"index":0,"delta":{"role":"assistant","content":null},"finish_reason":null}]}"#,
        ));

        // Tool call (xAI sends complete tool calls in single chunks)
        let events = handler.handle_event(&sse(
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1234,"model":"grok-3","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_abc","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"/tmp/test\"}"}}]},"finish_reason":null}]}"#,
        ));
        // Should have ToolCallStart + ToolCallDelta
        assert!(events.iter().any(|e| matches!(e, AssistantStreamEvent::ToolCallStart { .. })));

        // Finish with tool_calls reason
        handler.handle_event(&sse(
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","created":1234,"model":"grok-3","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        ));

        // [DONE]
        let events = handler.handle_event(&sse("[DONE]"));
        let has_tool_end = events.iter().any(|e| match e {
            AssistantStreamEvent::ToolCallEnd { tool_call, .. } => match tool_call {
                Content::ToolCall {
                    id,
                    name,
                    arguments,
                } => {
                    id == "call_abc"
                        && name == "read_file"
                        && arguments["path"] == "/tmp/test"
                }
                _ => false,
            },
            _ => false,
        });
        assert!(has_tool_end);
        assert!(matches!(
            events.last().unwrap(),
            AssistantStreamEvent::Done {
                stop_reason: StopReason::ToolUse
            }
        ));
    }

    #[test]
    fn test_incremental_tool_call_arguments() {
        let mut handler = ResponseHandler::new();

        // Start
        handler.handle_event(&sse(
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","model":"grok-3","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}"#,
        ));

        // Tool call start with name
        handler.handle_event(&sse(
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","model":"grok-3","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"bash","arguments":""}}]},"finish_reason":null}]}"#,
        ));

        // Incremental arguments
        handler.handle_event(&sse(
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","model":"grok-3","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"cmd\":"}}]},"finish_reason":null}]}"#,
        ));
        handler.handle_event(&sse(
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","model":"grok-3","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"ls\"}"}}]},"finish_reason":null}]}"#,
        ));

        // Finish
        handler.handle_event(&sse(
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","model":"grok-3","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        ));

        // Build final message
        let msg = handler.build_final_message();
        match msg {
            Message::Assistant { content, .. } => {
                assert_eq!(content.len(), 1);
                match &content[0] {
                    Content::ToolCall {
                        id,
                        name,
                        arguments,
                    } => {
                        assert_eq!(id, "call_1");
                        assert_eq!(name, "bash");
                        assert_eq!(arguments["cmd"], "ls");
                    }
                    _ => panic!("expected ToolCall"),
                }
            }
            _ => panic!("expected Assistant"),
        }
    }

    #[test]
    fn test_usage_parsing() {
        let mut handler = ResponseHandler::new();

        handler.handle_event(&sse(
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","model":"grok-3","choices":[{"index":0,"delta":{"role":"assistant","content":"Hi"},"finish_reason":null}]}"#,
        ));
        handler.handle_event(&sse(
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","model":"grok-3","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":50,"total_tokens":150}}"#,
        ));

        let msg = handler.build_final_message();
        match msg {
            Message::Assistant { usage, .. } => {
                assert_eq!(usage.input, 100);
                assert_eq!(usage.output, 50);
            }
            _ => panic!("expected Assistant"),
        }
    }

    #[test]
    fn test_done_signal() {
        let mut handler = ResponseHandler::new();
        let events = handler.handle_event(&sse("[DONE]"));
        assert_eq!(events.len(), 1); // Just Done (no text started)
        assert!(matches!(
            events[0],
            AssistantStreamEvent::Done { .. }
        ));
    }

    #[test]
    fn test_build_final_message_with_text_and_tool() {
        let mut handler = ResponseHandler::new();

        // Text content
        handler.handle_event(&sse(
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","model":"grok-3","choices":[{"index":0,"delta":{"role":"assistant","content":"Thinking..."},"finish_reason":null}]}"#,
        ));

        // Tool call
        handler.handle_event(&sse(
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","model":"grok-3","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read","arguments":"{\"path\":\"test.txt\"}"}}]},"finish_reason":null}]}"#,
        ));

        handler.handle_event(&sse(
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","model":"grok-3","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30}}"#,
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
                assert_eq!(model, "grok-3");
                assert_eq!(content.len(), 2);
                assert!(matches!(&content[0], Content::Text { text } if text == "Thinking..."));
                assert!(matches!(&content[1], Content::ToolCall { name, .. } if name == "read"));
                assert_eq!(usage.input, 10);
                assert_eq!(usage.output, 20);
                assert_eq!(stop_reason, StopReason::ToolUse);
            }
            _ => panic!("expected Assistant"),
        }
    }
}
