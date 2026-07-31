use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use rho_core::types::{AssistantStreamEvent, Content, Message, StopReason, Usage};
use serde_json::Value;

pub struct OpenAiResponseHandler {
    /// index → (id, name, accumulated_args)
    tool_call_accumulators: HashMap<usize, (String, String, String)>,
    text_started: bool,
    text_accumulated: String,
    started: bool,
    usage: Usage,
    model: String,
    stop_reason: StopReason,
}

impl Default for OpenAiResponseHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenAiResponseHandler {
    pub fn new() -> Self {
        Self {
            tool_call_accumulators: HashMap::new(),
            text_started: false,
            text_accumulated: String::new(),
            started: false,
            usage: Usage::default(),
            model: String::new(),
            stop_reason: StopReason::Stop,
        }
    }

    /// Process one parsed SSE data payload (already JSON-decoded).
    /// Returns any stream events produced by this chunk.
    ///
    /// Call with `None` when the stream ends so any pending state can be flushed.
    pub fn handle_chunk(&mut self, chunk: &Value) -> Vec<AssistantStreamEvent> {
        let mut events = Vec::new();

        // Emit Start once on the first real chunk
        if !self.started {
            self.started = true;
            events.push(AssistantStreamEvent::Start);
        }

        // Capture model string
        if let Some(model) = chunk.get("model").and_then(|v| v.as_str()) {
            if !model.is_empty() {
                self.model = model.to_string();
            }
        }

        // Capture usage when present (may arrive after choices)
        if let Some(usage) = chunk.get("usage") {
            self.usage = parse_usage(usage);
        }

        let choices = match chunk.get("choices").and_then(|v| v.as_array()) {
            Some(c) if !c.is_empty() => c,
            _ => return events,
        };

        let choice = &choices[0];

        // Text delta
        if let Some(content) = choice.pointer("/delta/content").and_then(|v| v.as_str()) {
            if !content.is_empty() {
                if !self.text_started {
                    self.text_started = true;
                    events.push(AssistantStreamEvent::TextStart { index: 0 });
                }
                self.text_accumulated.push_str(content);
                events.push(AssistantStreamEvent::TextDelta {
                    index: 0,
                    delta: content.to_string(),
                });
            }
        }

        // Tool call deltas
        if let Some(tool_calls) = choice
            .pointer("/delta/tool_calls")
            .and_then(|v| v.as_array())
        {
            for tc in tool_calls {
                let index = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

                // New tool call: has an id field
                if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                    let name = tc
                        .pointer("/function/name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    self.tool_call_accumulators
                        .insert(index, (id.to_string(), name.to_string(), String::new()));
                    events.push(AssistantStreamEvent::ToolCallStart { index });
                }

                // Arguments fragment
                if let Some(args_delta) = tc.pointer("/function/arguments").and_then(|v| v.as_str())
                {
                    if let Some(acc) = self.tool_call_accumulators.get_mut(&index) {
                        acc.2.push_str(args_delta);
                        events.push(AssistantStreamEvent::ToolCallDelta {
                            index,
                            delta: args_delta.to_string(),
                        });
                    }
                }
            }
        }

        // On finish_reason, record the stop reason but don't emit end events yet.
        // We wait for `flush()` (called on `[DONE]`) so any usage chunk that arrives
        // between finish_reason and [DONE] is captured before we emit Done.
        if let Some(finish_reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
            if !finish_reason.is_empty() {
                self.stop_reason = match finish_reason {
                    "stop" => StopReason::Stop,
                    "length" => StopReason::Length,
                    "tool_calls" => StopReason::ToolUse,
                    _ => StopReason::Stop,
                };
            }
        }

        events
    }

    /// Emit all closing events: TextEnd, ToolCallEnds, Done.
    ///
    /// Call this when the `[DONE]` sentinel is received. This ensures any usage
    /// chunk sent after `finish_reason` is captured before Done is emitted.
    pub fn flush(&mut self) -> Vec<AssistantStreamEvent> {
        let mut events = Vec::new();

        if !self.started {
            // Stream ended with no content at all — still emit Start + Done
            events.push(AssistantStreamEvent::Start);
        }

        // Close text block
        if self.text_started {
            events.push(AssistantStreamEvent::TextEnd {
                index: 0,
                content: self.text_accumulated.clone(),
            });
        }

        // Close all accumulated tool calls (sorted by index for determinism)
        let mut indices: Vec<usize> = self.tool_call_accumulators.keys().copied().collect();
        indices.sort_unstable();
        for idx in indices {
            let (id, name, args_str) = self.tool_call_accumulators[&idx].clone();
            let arguments = serde_json::from_str(&args_str)
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
            events.push(AssistantStreamEvent::ToolCallEnd {
                index: idx,
                tool_call: Content::ToolCall {
                    id,
                    name,
                    arguments,
                },
            });
        }

        events.push(AssistantStreamEvent::Done {
            stop_reason: self.stop_reason.clone(),
        });

        events
    }

    /// Build the final `Message::Assistant` from accumulated state.
    pub fn build_final_message(&self) -> Message {
        let mut content: Vec<Content> = Vec::new();

        if self.text_started && !self.text_accumulated.is_empty() {
            content.push(Content::Text {
                text: self.text_accumulated.clone(),
            });
        }

        let mut indices: Vec<usize> = self.tool_call_accumulators.keys().copied().collect();
        indices.sort_unstable();
        for idx in indices {
            let (id, name, args_str) = &self.tool_call_accumulators[&idx];
            let arguments = serde_json::from_str(args_str)
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
            content.push(Content::ToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments,
            });
        }

        Message::Assistant {
            content,
            model: self.model.clone(),
            usage: self.usage.clone(),
            stop_reason: self.stop_reason.clone(),
            timestamp: now_ms(),
        }
    }
}

fn parse_usage(usage: &Value) -> Usage {
    Usage {
        input: usage
            .get("prompt_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        output: usage
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        cache_read: 0,
        cache_write: 0,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_chunk(content: &str) -> Value {
        serde_json::json!({
            "choices": [{
                "delta": { "content": content },
                "finish_reason": null,
                "index": 0
            }],
            "model": "gpt-4o"
        })
    }

    fn finish_chunk(reason: &str) -> Value {
        serde_json::json!({
            "choices": [{
                "delta": {},
                "finish_reason": reason,
                "index": 0
            }]
        })
    }

    fn tool_start_chunk(index: usize, id: &str, name: &str) -> Value {
        serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": index,
                        "id": id,
                        "type": "function",
                        "function": { "name": name, "arguments": "" }
                    }]
                },
                "finish_reason": null,
                "index": 0
            }]
        })
    }

    fn tool_args_chunk(index: usize, args: &str) -> Value {
        serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": index,
                        "function": { "arguments": args }
                    }]
                },
                "finish_reason": null,
                "index": 0
            }]
        })
    }

    #[test]
    fn text_streaming() {
        let mut handler = OpenAiResponseHandler::new();

        let events = handler.handle_chunk(&text_chunk("Hello"));
        assert!(events
            .iter()
            .any(|e| matches!(e, AssistantStreamEvent::Start)));
        assert!(events
            .iter()
            .any(|e| matches!(e, AssistantStreamEvent::TextStart { index: 0 })));
        assert!(events.iter().any(|e| matches!(
            e,
            AssistantStreamEvent::TextDelta { index: 0, delta } if delta == "Hello"
        )));

        let events2 = handler.handle_chunk(&text_chunk(", world"));
        // Start and TextStart should not repeat
        assert!(!events2
            .iter()
            .any(|e| matches!(e, AssistantStreamEvent::Start)));
        assert!(!events2
            .iter()
            .any(|e| matches!(e, AssistantStreamEvent::TextStart { index: 0 })));
        assert!(events2.iter().any(|e| matches!(
            e,
            AssistantStreamEvent::TextDelta { index: 0, delta } if delta == ", world"
        )));

        // finish_reason only records stop_reason — no end events yet
        let events3 = handler.handle_chunk(&finish_chunk("stop"));
        assert!(!events3
            .iter()
            .any(|e| matches!(e, AssistantStreamEvent::TextEnd { .. })));
        assert!(!events3
            .iter()
            .any(|e| matches!(e, AssistantStreamEvent::Done { .. })));

        // flush() (called on [DONE]) emits the end events
        let flush_events = handler.flush();
        assert!(flush_events
            .iter()
            .any(|e| matches!(e, AssistantStreamEvent::TextEnd { index: 0, .. })));
        assert!(flush_events.iter().any(|e| matches!(
            e,
            AssistantStreamEvent::Done {
                stop_reason: StopReason::Stop
            }
        )));
    }

    #[test]
    fn tool_call_streaming() {
        let mut handler = OpenAiResponseHandler::new();

        let events1 = handler.handle_chunk(&tool_start_chunk(0, "call_abc", "bash"));
        assert!(events1
            .iter()
            .any(|e| matches!(e, AssistantStreamEvent::ToolCallStart { index: 0 })));

        let events2 = handler.handle_chunk(&tool_args_chunk(0, r#"{"command":"#));
        assert!(events2
            .iter()
            .any(|e| matches!(e, AssistantStreamEvent::ToolCallDelta { index: 0, .. })));

        let events3 = handler.handle_chunk(&tool_args_chunk(0, r#""ls"}"#));
        assert!(events3
            .iter()
            .any(|e| matches!(e, AssistantStreamEvent::ToolCallDelta { index: 0, .. })));

        // finish_reason only records stop_reason — end events come from flush()
        let events4 = handler.handle_chunk(&finish_chunk("tool_calls"));
        assert!(!events4
            .iter()
            .any(|e| matches!(e, AssistantStreamEvent::ToolCallEnd { .. })));
        assert!(!events4
            .iter()
            .any(|e| matches!(e, AssistantStreamEvent::Done { .. })));

        let flush_events = handler.flush();
        let end_event = flush_events
            .iter()
            .find(|e| matches!(e, AssistantStreamEvent::ToolCallEnd { .. }))
            .expect("should emit ToolCallEnd");

        if let AssistantStreamEvent::ToolCallEnd { index, tool_call } = end_event {
            assert_eq!(*index, 0);
            if let Content::ToolCall {
                id,
                name,
                arguments,
            } = tool_call
            {
                assert_eq!(id, "call_abc");
                assert_eq!(name, "bash");
                assert_eq!(arguments["command"], "ls");
            } else {
                panic!("expected ToolCall content");
            }
        }

        assert!(flush_events.iter().any(|e| matches!(
            e,
            AssistantStreamEvent::Done {
                stop_reason: StopReason::ToolUse
            }
        )));
    }

    #[test]
    fn multiple_tool_calls() {
        let mut handler = OpenAiResponseHandler::new();

        handler.handle_chunk(&tool_start_chunk(0, "call_1", "read"));
        handler.handle_chunk(&tool_args_chunk(0, r#"{"path":"a.txt"}"#));
        handler.handle_chunk(&tool_start_chunk(1, "call_2", "bash"));
        handler.handle_chunk(&tool_args_chunk(1, r#"{"command":"ls"}"#));

        handler.handle_chunk(&finish_chunk("tool_calls"));
        let close_events = handler.flush();

        let tool_ends: Vec<_> = close_events
            .iter()
            .filter(|e| matches!(e, AssistantStreamEvent::ToolCallEnd { .. }))
            .collect();
        assert_eq!(tool_ends.len(), 2);

        // Should be sorted by index
        if let AssistantStreamEvent::ToolCallEnd {
            index: i0,
            tool_call: tc0,
        } = &tool_ends[0]
        {
            assert_eq!(*i0, 0);
            if let Content::ToolCall { name, .. } = tc0 {
                assert_eq!(name, "read");
            }
        }
        if let AssistantStreamEvent::ToolCallEnd {
            index: i1,
            tool_call: tc1,
        } = &tool_ends[1]
        {
            assert_eq!(*i1, 1);
            if let Content::ToolCall { name, .. } = tc1 {
                assert_eq!(name, "bash");
            }
        }
    }

    #[test]
    fn usage_extraction() {
        let mut handler = OpenAiResponseHandler::new();
        let usage_chunk = serde_json::json!({
            "choices": [],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 50,
                "total_tokens": 150
            }
        });

        handler.handle_chunk(&text_chunk("hi"));
        handler.handle_chunk(&finish_chunk("stop"));
        handler.handle_chunk(&usage_chunk);

        let msg = handler.build_final_message();
        if let Message::Assistant { usage, .. } = msg {
            assert_eq!(usage.input, 100);
            assert_eq!(usage.output, 50);
        } else {
            panic!("expected Assistant message");
        }
    }

    #[test]
    fn build_final_message_text_only() {
        let mut handler = OpenAiResponseHandler::new();
        handler.handle_chunk(&text_chunk("Hello"));
        handler.handle_chunk(&text_chunk(" world"));
        handler.handle_chunk(&finish_chunk("stop"));

        let msg = handler.build_final_message();
        if let Message::Assistant {
            content,
            stop_reason,
            ..
        } = msg
        {
            assert_eq!(content.len(), 1);
            if let Content::Text { text } = &content[0] {
                assert_eq!(text, "Hello world");
            }
            assert_eq!(stop_reason, StopReason::Stop);
        }
    }

    #[test]
    fn length_stop_reason() {
        let mut handler = OpenAiResponseHandler::new();
        handler.handle_chunk(&text_chunk("..."));
        handler.handle_chunk(&finish_chunk("length"));

        let msg = handler.build_final_message();
        if let Message::Assistant { stop_reason, .. } = msg {
            assert_eq!(stop_reason, StopReason::Length);
        }
    }
}
