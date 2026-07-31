use futures::StreamExt;
use rho_core::event_stream::{EventStream, EventStreamProducer};
use rho_core::provider_types::{AssistantStream, StreamContext, StreamFn, StreamOptions};
use rho_core::types::{AssistantStreamEvent, Message, Model, StopReason};

use crate::error::ProviderError;
use crate::openai_response::OpenAiResponseHandler;

/// Returns a closure that streams OpenAI-compatible API responses.
///
/// Compatible with OpenAI, xAI/Grok, Groq, Ollama, and Google Gemini
/// (via their OpenAI-compat endpoint).
///
/// `server_tools` are provider-managed tools (e.g. xAI's `web_search`, `x_search`) that
/// are injected into the request as `{"type": "<name>"}` entries. They run on the
/// provider's servers and do not correspond to local tool definitions.
pub fn openai_stream_fn(server_tools: Option<Vec<String>>) -> StreamFn {
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
    match do_stream_inner(
        &model,
        &context,
        &options,
        server_tools.as_deref(),
        &producer,
    )
    .await
    {
        Ok(msg) => {
            producer.end(Some(msg));
        }
        Err(e) => {
            tracing::error!("OpenAI provider stream error: {}", e);
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
    let body = crate::openai_request::build_request_body(model, context, options, server_tools);

    let client = reqwest::Client::new();
    let base_url = if model.base_url.is_empty() {
        "https://api.openai.com/v1"
    } else {
        model.base_url.trim_end_matches('/')
    };

    let resp = client
        .post(format!("{}/chat/completions", base_url))
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

    let byte_stream = resp.bytes_stream();
    let mut sse_stream = std::pin::pin!(crate::sse::parse_sse_stream(byte_stream));
    let mut handler = OpenAiResponseHandler::new();

    while let Some(event_result) = sse_stream.next().await {
        match event_result {
            Ok(sse_event) => {
                // OpenAI SSE uses bare `data:` lines (no event: field).
                // The sentinel `data: [DONE]` marks stream end — flush all pending events.
                if sse_event.data == "[DONE]" {
                    let flush_events = handler.flush();
                    for event in flush_events {
                        let _ = producer.push(event).await;
                    }
                    break;
                }

                match serde_json::from_str::<serde_json::Value>(&sse_event.data) {
                    Ok(chunk) => {
                        let events = handler.handle_chunk(&chunk);
                        for event in events {
                            let _ = producer.push(event).await;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "OpenAI SSE JSON parse error: {} (data: {})",
                            e,
                            sse_event.data
                        );
                    }
                }
            }
            Err(e) => {
                tracing::error!("OpenAI SSE stream error: {}", e);
            }
        }
    }

    Ok(handler.build_final_message())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_stream_fn_compiles() {
        let _f: StreamFn = openai_stream_fn(None);
    }
}
