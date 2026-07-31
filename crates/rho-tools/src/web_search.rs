// Web Search tool — search via DuckDuckGo (no API key required)
// Optionally proxies through `claude -p` with built-in WebSearch tool.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use rho_core::tool::{AgentTool, ToolError};
use rho_core::types::{Content, ToolResult};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

const DEFAULT_LIMIT: usize = 5;
const REQUEST_TIMEOUT_SECS: u64 = 15;

pub struct WebSearchTool {
    claude_proxy: Option<Arc<AtomicBool>>,
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl WebSearchTool {
    pub fn new() -> Self {
        Self { claude_proxy: None }
    }

    pub fn with_claude_proxy(flag: Arc<AtomicBool>) -> Self {
        Self {
            claude_proxy: Some(flag),
        }
    }

    fn use_claude(&self) -> bool {
        self.claude_proxy
            .as_ref()
            .is_some_and(|f| f.load(Ordering::Relaxed))
    }
}

/// Parse DuckDuckGo HTML search results into (title, url, snippet) tuples.
fn parse_ddg_results(html: &str) -> Vec<(String, String, String)> {
    let mut results = Vec::new();

    // Each result has:
    //   <a class="result__a" href="URL">TITLE</a>
    //   <a class="result__snippet" href="...">SNIPPET</a>
    let re_link =
        regex::Regex::new(r#"<a[^>]*class="result__a"[^>]*href="([^"]*)"[^>]*>(.*?)</a>"#).unwrap();
    let re_snippet = regex::Regex::new(r#"<a[^>]*class="result__snippet"[^>]*>(.*?)</a>"#).unwrap();

    let links: Vec<_> = re_link.captures_iter(html).collect();
    let snippets: Vec<_> = re_snippet.captures_iter(html).collect();

    for (i, link_cap) in links.iter().enumerate() {
        let url = link_cap[1].to_string();
        let title = strip_html_tags(&link_cap[2]);
        let snippet = snippets
            .get(i)
            .map(|s| strip_html_tags(&s[1]))
            .unwrap_or_default();
        results.push((title, url, snippet));
    }

    results
}

/// Remove HTML tags from a string.
fn strip_html_tags(s: &str) -> String {
    let re = regex::Regex::new(r"<[^>]+>").unwrap();
    let text = re.replace_all(s, "");
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .trim()
        .to_string()
}

/// Execute a web search via `claude -p` with the built-in WebSearch tool.
async fn search_via_claude(query: &str, limit: usize) -> Result<ToolResult, ToolError> {
    let prompt = format!(
        "Search the web for: {query}\n\n\
         Return exactly the top {limit} results as a numbered list. \
         For each result, include:\n\
         1. Title\n\
         2. URL\n\
         3. A brief snippet/description\n\n\
         Format each result as:\n\
         N. TITLE\n   URL\n   SNIPPET\n\n\
         Include a Sources: section at the end with URLs."
    );

    let output = tokio::process::Command::new("claude")
        .arg("-p")
        .arg(&prompt)
        .arg("--tools")
        .arg("WebSearch,WebFetch")
        .arg("--permission-mode")
        .arg("bypassPermissions")
        .arg("--model")
        .arg("haiku")
        .arg("--no-session-persistence")
        .env("CLAUDECODE", "")
        .output()
        .await
        .map_err(|e| {
            ToolError::ExecutionFailed(format!(
                "failed to run claude CLI: {e}. Is Claude Code installed?"
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ToolError::ExecutionFailed(format!(
            "claude exited with {}: {}",
            output.status,
            stderr.chars().take(500).collect::<String>()
        )));
    }

    let text = String::from_utf8_lossy(&output.stdout).to_string();
    if text.trim().is_empty() {
        return Ok(ToolResult {
            content: vec![Content::Text {
                text: format!("No results found for: {query}"),
            }],
            details: serde_json::json!({"source": "claude-proxy", "query": query}),
        });
    }

    Ok(ToolResult {
        content: vec![Content::Text { text }],
        details: serde_json::json!({"source": "claude-proxy", "query": query}),
    })
}

#[async_trait]
impl AgentTool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn is_concurrent_safe(&self) -> bool {
        true
    }

    fn label(&self) -> String {
        "Web Search".to_string()
    }

    fn description(&self) -> String {
        "Search the web via DuckDuckGo and return results (title, URL, snippet).".to_string()
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results (default 5, max 20)"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(
        &self,
        _tool_call_id: &str,
        params: Value,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::InvalidParameters("missing or invalid 'query' parameter".into())
            })?;

        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| (v as usize).min(20))
            .unwrap_or(DEFAULT_LIMIT);

        // Claude proxy mode: shell out to `claude -p`
        if self.use_claude() {
            return search_via_claude(query, limit).await;
        }

        // Native DuckDuckGo mode
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .build()
            .map_err(|e| ToolError::ExecutionFailed(format!("failed to build HTTP client: {e}")))?;

        // DuckDuckGo HTML endpoint via POST (works without API key)
        let response = client
            .post("https://html.duckduckgo.com/html/")
            .header("Referer", "https://duckduckgo.com/")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!("q={}&b=&kl=", urlencod(query)))
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("search request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            return Ok(ToolResult {
                content: vec![Content::Text {
                    text: format!("DuckDuckGo returned HTTP {status}"),
                }],
                details: serde_json::json!({"status": status.as_u16()}),
            });
        }

        let html = response.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("failed to read search response: {e}"))
        })?;

        let results = parse_ddg_results(&html);

        if results.is_empty() {
            return Ok(ToolResult {
                content: vec![Content::Text {
                    text: format!("No results found for: {query}"),
                }],
                details: serde_json::json!({"query": query, "result_count": 0}),
            });
        }

        let mut output = String::new();
        for (i, (title, url, snippet)) in results.iter().take(limit).enumerate() {
            if i > 0 {
                output.push('\n');
            }
            output.push_str(&format!(
                "{}. {}\n   {}\n   {}\n",
                i + 1,
                title,
                url,
                snippet
            ));
        }

        Ok(ToolResult {
            content: vec![Content::Text { text: output }],
            details: serde_json::json!({"query": query, "result_count": results.len().min(limit)}),
        })
    }
}

/// Percent-encode a query string for use in form data.
fn urlencod(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(b as char);
            }
            b' ' => result.push('+'),
            _ => {
                result.push_str(&format!("%{:02X}", b));
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ddg_html() {
        let html = r#"
        <div class="result">
            <a class="result__a" href="https://example.com">Example <b>Title</b></a>
            <a class="result__snippet" href="https://example.com">This is a <b>snippet</b> of text.</a>
        </div>
        <div class="result">
            <a class="result__a" href="https://other.com">Other Result</a>
            <a class="result__snippet" href="https://other.com">Another snippet here.</a>
        </div>
        "#;
        let results = parse_ddg_results(html);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "Example Title");
        assert_eq!(results[0].1, "https://example.com");
        assert!(results[0].2.contains("snippet"));
        assert_eq!(results[1].0, "Other Result");
    }

    #[test]
    fn test_urlencod() {
        assert_eq!(urlencod("hello world"), "hello+world");
        assert_eq!(urlencod("rust+iced"), "rust%2Biced");
        assert_eq!(urlencod("a&b=c"), "a%26b%3Dc");
    }

    #[test]
    fn strip_tags() {
        assert_eq!(strip_html_tags("<b>bold</b> text"), "bold text");
        assert_eq!(strip_html_tags("no tags"), "no tags");
        assert_eq!(strip_html_tags("&amp; &lt;"), "& <");
    }
}
