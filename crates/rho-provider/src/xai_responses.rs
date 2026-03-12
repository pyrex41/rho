use rho_core::event_stream::{EventStream, EventStreamProducer};
use rho_core::provider_types::{AssistantStream, StreamContext, StreamFn, StreamOptions};
use rho_core::types::{
    AssistantStreamEvent, Content, Message, Model, StopReason, Usage, UserContent,
};

use crate::error::ProviderError;

/// Returns a closure that calls xAI's Responses API (non-streaming).
///
/// The Responses API is required for multi-agent models which are rejected
/// by the chat completions endpoint. It uses `input` instead of `messages`,
/// `developer` role instead of `system`, and returns a single JSON response.
pub fn xai_responses_stream_fn(server_tools: Option<Vec<String>>) -> StreamFn {
    std::sync::Arc::new(
        move |model: &Model, context: StreamContext, options: StreamOptions| {
            let model = model.clone();
            let server_tools = server_tools.clone();
            let stream: AssistantStream = EventStream::new();
            let (producer, consumer) = stream.split();

            tokio::spawn(async move {
                do_stream(model, context, options, server_tools, producer).await;
            });

            consumer
        },
    )
}

async fn do_stream(
    model: Model,
    context: StreamContext,
    options: StreamOptions,
    server_tools: Option<Vec<String>>,
    mut producer: EventStreamProducer<AssistantStreamEvent, Message>,
) {
    match do_stream_inner(&model, &context, &options, server_tools.as_deref(), &producer).await {
        Ok(msg) => {
            producer.end(Some(msg));
        }
        Err(e) => {
            tracing::error!("xAI Responses API error: {}", e);
            let _ = producer
                .push(AssistantStreamEvent::Error {
                    stop_reason: StopReason::Error,
                })
                .await;
            producer.end(None);
        }
    }
}

async fn do_stream_inner(
    model: &Model,
    context: &StreamContext,
    options: &StreamOptions,
    server_tools: Option<&[String]>,
    producer: &EventStreamProducer<AssistantStreamEvent, Message>,
) -> Result<Message, ProviderError> {
    let mut input: Vec<serde_json::Value> = Vec::new();

    // System prompt → developer role
    if let Some(ref system) = options.system_prompt {
        input.push(serde_json::json!({
            "role": "developer",
            "content": system,
        }));
    }

    // Conversation messages (user/assistant only — no tool results)
    for msg in &context.messages {
        if let Some(converted) = convert_message(msg) {
            input.push(converted);
        }
    }

    let mut body = serde_json::json!({
        "model": model.id,
        "input": input,
        "reasoning": { "effort": "low" },
    });

    // Server-side tools
    if let Some(st) = server_tools {
        let tools: Vec<serde_json::Value> = st
            .iter()
            .map(|name| serde_json::json!({ "type": name }))
            .collect();
        if !tools.is_empty() {
            body["tools"] = serde_json::Value::Array(tools);
        }
    }

    let client = reqwest::Client::new();
    let base_url = model.base_url.trim_end_matches('/');

    let resp = client
        .post(format!("{}/responses", base_url))
        .header("Authorization", format!("Bearer {}", options.api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(ProviderError::ApiError { status, body });
    }

    let response: serde_json::Value = resp.json().await?;

    // Extract text from response.output[].content[].text
    let mut text = String::new();
    if let Some(output) = response["output"].as_array() {
        for item in output {
            if item["type"].as_str() == Some("message") {
                if let Some(content) = item["content"].as_array() {
                    for block in content {
                        if block["type"].as_str() == Some("output_text") {
                            if let Some(t) = block["text"].as_str() {
                                text.push_str(t);
                            }
                        }
                    }
                }
            }
        }
    }

    // Extract usage
    let usage = Usage {
        input: response["usage"]["input_tokens"].as_u64().unwrap_or(0),
        output: response["usage"]["output_tokens"].as_u64().unwrap_or(0),
        cache_read: 0,
        cache_write: 0,
    };

    // Emit events to match the streaming protocol
    let _ = producer.push(AssistantStreamEvent::Start).await;
    let _ = producer
        .push(AssistantStreamEvent::TextStart { index: 0 })
        .await;
    let _ = producer
        .push(AssistantStreamEvent::TextDelta {
            index: 0,
            delta: text.clone(),
        })
        .await;
    let _ = producer
        .push(AssistantStreamEvent::TextEnd {
            index: 0,
            content: text.clone(),
        })
        .await;
    let _ = producer
        .push(AssistantStreamEvent::Done {
            stop_reason: StopReason::Stop,
        })
        .await;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    Ok(Message::Assistant {
        content: vec![Content::Text { text }],
        model: model.id.clone(),
        usage,
        stop_reason: StopReason::Stop,
        timestamp: now,
    })
}

fn convert_message(msg: &Message) -> Option<serde_json::Value> {
    match msg {
        Message::User { content, .. } => {
            let text = match content {
                UserContent::Text(s) => s.clone(),
                UserContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|b| match b {
                        Content::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(""),
            };
            Some(serde_json::json!({
                "role": "user",
                "content": text,
            }))
        }
        Message::Assistant { content, .. } => {
            let text: String = content
                .iter()
                .filter_map(|c| match c {
                    Content::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            Some(serde_json::json!({
                "role": "assistant",
                "content": text,
            }))
        }
        Message::ToolResult { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xai_responses_stream_fn_compiles() {
        let _f: StreamFn = xai_responses_stream_fn(None);
    }

    #[test]
    fn xai_responses_stream_fn_with_server_tools_compiles() {
        let _f: StreamFn = xai_responses_stream_fn(Some(vec!["web_search".into()]));
    }
}
