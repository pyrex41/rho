// Web Fetch tool — fetch a URL and return its content as markdown/text

use async_trait::async_trait;
use rho_core::tool::{AgentTool, ToolError};
use rho_core::types::{Content, ToolResult};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

const DEFAULT_MAX_CHARS: usize = 50_000;
const TRUNCATION_EDGE: usize = 5_000;
const REQUEST_TIMEOUT_SECS: u64 = 30;

pub struct WebFetchTool;

impl WebFetchTool {
    pub fn new() -> Self {
        Self
    }
}

/// Strip HTML tags and decode common entities to produce readable plain text.
fn html_to_text(html: &str) -> String {
    // Remove script and style blocks entirely
    let re_script = regex::Regex::new(r"(?is)<script[^>]*>.*?</script>").unwrap();
    let cleaned = re_script.replace_all(html, "");
    let re_style = regex::Regex::new(r"(?is)<style[^>]*>.*?</style>").unwrap();
    let cleaned = re_style.replace_all(&cleaned, "");

    // Remove all HTML tags
    let re_tags = regex::Regex::new(r"<[^>]+>").unwrap();
    let text = re_tags.replace_all(&cleaned, " ");

    // Decode common HTML entities
    let text = text
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ");

    // Collapse whitespace
    let re_ws = regex::Regex::new(r"[ \t]+").unwrap();
    let text = re_ws.replace_all(&text, " ");

    // Collapse multiple newlines
    let re_nl = regex::Regex::new(r"\n{3,}").unwrap();
    let text = re_nl.replace_all(&text, "\n\n");

    text.trim().to_string()
}

/// Truncate text that exceeds max_chars, keeping head and tail.
fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }

    let head_end = {
        let mut end = TRUNCATION_EDGE.min(max_chars / 2);
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        end
    };

    let tail_start = {
        let mut start = text.len().saturating_sub(TRUNCATION_EDGE.min(max_chars / 2));
        while start < text.len() && !text.is_char_boundary(start) {
            start += 1;
        }
        start
    };

    format!(
        "{}\n\n[...truncated ({} chars total)...]\n\n{}",
        &text[..head_end],
        text.len(),
        &text[tail_start..],
    )
}

#[async_trait]
impl AgentTool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn label(&self) -> String {
        "Web Fetch".to_string()
    }

    fn description(&self) -> String {
        "Fetch a URL and return its content as clean markdown/text.".to_string()
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch"
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Maximum characters to return (default 50000)"
                },
                "raw": {
                    "type": "boolean",
                    "description": "If true, fetch directly instead of using the markdown reader (default false)"
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(
        &self,
        _tool_call_id: &str,
        params: Value,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let url = params
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::InvalidParameters("missing or invalid 'url' parameter".into())
            })?;

        let max_chars = params
            .get("max_chars")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(DEFAULT_MAX_CHARS);

        let raw = params
            .get("raw")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .user_agent("rho/0.1")
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|e| ToolError::ExecutionFailed(format!("failed to build HTTP client: {e}")))?;

        if !raw {
            // Try markdown.new reader service first — returns clean markdown
            let reader_url = format!("https://markdown.new/{}", url);
            match client.get(&reader_url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    let text = resp.text().await.map_err(|e| {
                        ToolError::ExecutionFailed(format!("failed to read response: {e}"))
                    })?;
                    if !text.is_empty() {
                        let text = truncate_text(&text, max_chars);
                        return Ok(ToolResult {
                            content: vec![Content::Text { text }],
                            details: serde_json::json!({"source": "markdown.new"}),
                        });
                    }
                }
                _ => {} // Fall through to direct fetch
            }
        }

        // Direct fetch fallback
        let response = client
            .get(url)
            .header("Accept", "text/markdown, text/html;q=0.9, */*;q=0.8")
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            return Ok(ToolResult {
                content: vec![Content::Text {
                    text: format!("HTTP {status} for {url}"),
                }],
                details: serde_json::json!({"status": status.as_u16()}),
            });
        }

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let body = response.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("failed to read response body: {e}"))
        })?;

        // If we got markdown back (from Cloudflare's markdown-for-agents), use it directly
        let text = if content_type.contains("markdown") {
            body
        } else if content_type.contains("html") {
            html_to_text(&body)
        } else {
            body
        };

        let text = truncate_text(&text, max_chars);

        Ok(ToolResult {
            content: vec![Content::Text { text }],
            details: serde_json::json!({"source": "direct", "content_type": content_type}),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_stripping() {
        let html = r#"<html><head><title>Test</title><style>body{}</style></head>
            <body><h1>Hello</h1><p>World &amp; friends</p>
            <script>alert('x')</script></body></html>"#;
        let text = html_to_text(html);
        assert!(text.contains("Hello"));
        assert!(text.contains("World & friends"));
        assert!(!text.contains("<h1>"));
        assert!(!text.contains("alert"));
        assert!(!text.contains("body{}"));
    }

    #[test]
    fn truncation() {
        let long_text = "a".repeat(100_000);
        let truncated = truncate_text(&long_text, 20_000);
        assert!(truncated.len() < 100_000);
        assert!(truncated.contains("[...truncated"));
    }

    #[test]
    fn no_truncation_for_short_text() {
        let short = "hello world";
        assert_eq!(truncate_text(short, 50_000), short);
    }
}
