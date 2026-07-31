use bytes::Bytes;
use futures::Stream;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::error::ProviderError;

#[derive(Debug, Clone, PartialEq)]
pub struct SseEvent {
    pub event_type: String,
    pub data: String,
}

pub fn parse_sse_stream(
    body: impl Stream<Item = Result<Bytes, reqwest::Error>> + Unpin + Send + 'static,
) -> impl Stream<Item = Result<SseEvent, ProviderError>> {
    SseParser {
        inner: Box::pin(body),
        buffer: String::new(),
    }
}

struct SseParser {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Unpin + Send>>,
    buffer: String,
}

impl Stream for SseParser {
    type Item = Result<SseEvent, ProviderError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            // Try to extract a complete frame from the buffer
            if let Some(event) = try_parse_frame(&mut this.buffer) {
                return Poll::Ready(Some(Ok(event)));
            }

            // Need more data from the byte stream
            match this.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    match std::str::from_utf8(&bytes) {
                        Ok(s) => this.buffer.push_str(s),
                        Err(e) => {
                            return Poll::Ready(Some(Err(ProviderError::SseParse(format!(
                                "Invalid UTF-8: {e}"
                            )))));
                        }
                    }
                    // Loop to try parsing again with new data
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(ProviderError::Http(e))));
                }
                Poll::Ready(None) => {
                    // Stream ended. Try to parse any remaining data in buffer.
                    if this.buffer.trim().is_empty() {
                        return Poll::Ready(None);
                    }
                    // Try to parse remaining buffer as a final frame
                    if let Some(event) = try_parse_frame_final(&mut this.buffer) {
                        return Poll::Ready(Some(Ok(event)));
                    }
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Try to extract a complete SSE frame (terminated by \n\n) from the buffer.
/// Returns None if no complete frame is available yet.
fn try_parse_frame(buffer: &mut String) -> Option<SseEvent> {
    let boundary = buffer.find("\n\n")?;
    let frame = buffer[..boundary].to_string();
    *buffer = buffer[boundary + 2..].to_string();
    parse_frame(&frame)
}

/// Try to parse remaining buffer content as a final frame when stream ends.
fn try_parse_frame_final(buffer: &mut String) -> Option<SseEvent> {
    let frame = std::mem::take(buffer);
    let trimmed = frame.trim();
    if trimmed.is_empty() {
        return None;
    }
    parse_frame(trimmed)
}

/// Parse an SSE frame string into an SseEvent.
fn parse_frame(frame: &str) -> Option<SseEvent> {
    let mut event_type = String::new();
    let mut data_lines = Vec::new();

    for line in frame.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with(':') {
            // Comment line or empty line within a frame, skip
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event_type = value.trim().to_string();
        } else if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(value.trim().to_string());
        } else if line.starts_with("id:") || line.starts_with("retry:") {
            // Ignore id and retry fields
        }
    }

    if event_type.is_empty() && data_lines.is_empty() {
        return None;
    }

    Some(SseEvent {
        event_type,
        data: data_lines.join("\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    fn bytes_stream(
        chunks: Vec<&str>,
    ) -> impl Stream<Item = Result<Bytes, reqwest::Error>> + Unpin {
        futures::stream::iter(
            chunks
                .into_iter()
                .map(|s| Ok(Bytes::from(s.to_string())))
                .collect::<Vec<_>>(),
        )
    }

    #[tokio::test]
    async fn test_single_event() {
        let body = bytes_stream(vec![
            "event: message_start\ndata: {\"type\":\"message\"}\n\n",
        ]);
        let mut stream = std::pin::pin!(parse_sse_stream(body));

        let event = stream.next().await.unwrap().unwrap();
        assert_eq!(event.event_type, "message_start");
        assert_eq!(event.data, "{\"type\":\"message\"}");

        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_multiple_events() {
        let body = bytes_stream(vec![
            "event: ping\ndata: {}\n\nevent: message_start\ndata: {\"hello\":true}\n\n",
        ]);
        let mut stream = std::pin::pin!(parse_sse_stream(body));

        let e1 = stream.next().await.unwrap().unwrap();
        assert_eq!(e1.event_type, "ping");

        let e2 = stream.next().await.unwrap().unwrap();
        assert_eq!(e2.event_type, "message_start");
        assert_eq!(e2.data, "{\"hello\":true}");

        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_partial_frames() {
        let body = bytes_stream(vec![
            "event: content",
            "_block_start\ndata: {\"index\":",
            "0}\n\n",
        ]);
        let mut stream = std::pin::pin!(parse_sse_stream(body));

        let event = stream.next().await.unwrap().unwrap();
        assert_eq!(event.event_type, "content_block_start");
        assert_eq!(event.data, "{\"index\":0}");
    }

    #[tokio::test]
    async fn test_comment_lines_skipped() {
        let body = bytes_stream(vec![": this is a comment\nevent: ping\ndata: {}\n\n"]);
        let mut stream = std::pin::pin!(parse_sse_stream(body));

        let event = stream.next().await.unwrap().unwrap();
        assert_eq!(event.event_type, "ping");
    }

    #[tokio::test]
    async fn test_multiple_data_lines() {
        let body = bytes_stream(vec!["event: test\ndata: line1\ndata: line2\n\n"]);
        let mut stream = std::pin::pin!(parse_sse_stream(body));

        let event = stream.next().await.unwrap().unwrap();
        assert_eq!(event.data, "line1\nline2");
    }
}
