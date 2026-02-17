// Web Search tool — search the web using Brave Search API

use async_trait::async_trait;
use rho_core::tool::{AgentTool, ToolError};
use rho_core::types::{Content, ToolResult};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

const DEFAULT_LIMIT: usize = 5;
const REQUEST_TIMEOUT_SECS: u64 = 15;

pub struct WebSearchTool;

impl WebSearchTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl AgentTool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn label(&self) -> String {
        "Web Search".to_string()
    }

    fn description(&self) -> String {
        "Search the web and return results (title, URL, snippet). Uses Brave Search API."
            .to_string()
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

        let api_key = std::env::var("BRAVE_SEARCH_API_KEY").ok();
        let api_key = match api_key {
            Some(key) if !key.is_empty() => key,
            _ => {
                return Ok(ToolResult {
                    content: vec![Content::Text {
                        text: "Web search is not configured. Set the BRAVE_SEARCH_API_KEY \
                               environment variable to enable web search.\n\n\
                               Get a free API key at: https://brave.com/search/api/"
                            .to_string(),
                    }],
                    details: serde_json::json!({"configured": false}),
                });
            }
        };

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .map_err(|e| ToolError::ExecutionFailed(format!("failed to build HTTP client: {e}")))?;

        let response = client
            .get("https://api.search.brave.com/res/v1/web/search")
            .header("X-Subscription-Token", &api_key)
            .header("Accept", "application/json")
            .query(&[("q", query), ("count", &limit.to_string())])
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("search request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Ok(ToolResult {
                content: vec![Content::Text {
                    text: format!("Brave Search API returned HTTP {status}: {body}"),
                }],
                details: serde_json::json!({"status": status.as_u16()}),
            });
        }

        let body: Value = response.json().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("failed to parse search response: {e}"))
        })?;

        let results = body
            .get("web")
            .and_then(|w| w.get("results"))
            .and_then(|r| r.as_array());

        let text = match results {
            Some(results) if !results.is_empty() => {
                let mut output = String::new();
                for (i, result) in results.iter().take(limit).enumerate() {
                    let title = result.get("title").and_then(|v| v.as_str()).unwrap_or("");
                    let url = result.get("url").and_then(|v| v.as_str()).unwrap_or("");
                    let description = result
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    if i > 0 {
                        output.push('\n');
                    }
                    output.push_str(&format!(
                        "{}. {}\n   {}\n   {}\n",
                        i + 1,
                        title,
                        url,
                        description
                    ));
                }
                output
            }
            _ => format!("No results found for: {query}"),
        };

        Ok(ToolResult {
            content: vec![Content::Text { text }],
            details: serde_json::json!({"query": query, "result_count": results.map(|r| r.len()).unwrap_or(0)}),
        })
    }
}
