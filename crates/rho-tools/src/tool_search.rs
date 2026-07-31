use std::sync::Arc;

use async_trait::async_trait;
use rho_core::tool::{AgentTool, ToolError};
use rho_core::types::{Content, ToolResult};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// Tool that allows the model to look up full schemas for deferred tools.
/// When many tools are registered and some are deferred (name-only), the model
/// calls this tool to get the full schema before invoking a deferred tool.
pub struct ToolSearchTool {
    tools: Arc<Vec<Arc<dyn AgentTool>>>,
}

impl ToolSearchTool {
    pub fn new(tools: Arc<Vec<Arc<dyn AgentTool>>>) -> Self {
        Self { tools }
    }
}

#[async_trait]
impl AgentTool for ToolSearchTool {
    fn name(&self) -> &str {
        "tool_search"
    }

    fn label(&self) -> String {
        "Tool Search".to_string()
    }

    fn description(&self) -> String {
        "Search for and retrieve full tool schemas. Use 'select:name1,name2' for exact \
         name lookup, or provide keywords to search tool names and descriptions."
            .to_string()
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Tool name or keyword to search for. Use 'select:name1,name2' for exact match."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 5)"
                }
            }
        })
    }

    fn is_concurrent_safe(&self) -> bool {
        true
    }

    // ToolSearch itself is never deferred
    fn is_deferrable(&self) -> bool {
        false
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
            .ok_or_else(|| ToolError::InvalidParameters("query is required".into()))?;

        let max_results = params
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;

        let matched = if let Some(names) = query.strip_prefix("select:") {
            // Exact name match
            let requested: Vec<&str> = names.split(',').map(|s| s.trim()).collect();
            self.tools
                .iter()
                .filter(|t| requested.contains(&t.name()))
                .collect::<Vec<_>>()
        } else {
            // Keyword search
            let query_lower = query.to_lowercase();
            let mut scored: Vec<(&Arc<dyn AgentTool>, u32)> = self
                .tools
                .iter()
                .filter_map(|t| {
                    let name = t.name().to_lowercase();
                    let desc = t.description().to_lowercase();
                    let mut score = 0u32;

                    if name == query_lower {
                        score += 10;
                    } else if name.contains(&query_lower) {
                        score += 5;
                    }
                    if desc.contains(&query_lower) {
                        score += 2;
                    }

                    if score > 0 {
                        Some((t, score))
                    } else {
                        None
                    }
                })
                .collect();

            scored.sort_by_key(|entry| std::cmp::Reverse(entry.1));
            scored.into_iter().map(|(t, _)| t).collect()
        };

        let results: Vec<Value> = matched
            .into_iter()
            .take(max_results)
            .map(|t| {
                serde_json::json!({
                    "name": t.name(),
                    "description": t.description(),
                    "parameters": t.parameters_schema(),
                })
            })
            .collect();

        let text = if results.is_empty() {
            format!("No tools found matching '{}'", query)
        } else {
            serde_json::to_string_pretty(&results).unwrap_or_else(|_| "[]".to_string())
        };

        Ok(ToolResult {
            content: vec![Content::Text { text }],
            details: serde_json::json!({"count": results.len()}),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rho_core::tool::ToolError;

    struct MockSearchableTool {
        tool_name: String,
        tool_description: String,
    }

    #[async_trait]
    impl AgentTool for MockSearchableTool {
        fn name(&self) -> &str {
            &self.tool_name
        }
        fn label(&self) -> String {
            self.tool_name.clone()
        }
        fn description(&self) -> String {
            self.tool_description.clone()
        }
        fn parameters_schema(&self) -> Value {
            serde_json::json!({"type": "object"})
        }
        fn is_deferrable(&self) -> bool {
            true
        }
        async fn execute(
            &self,
            _: &str,
            _: Value,
            _: CancellationToken,
        ) -> Result<ToolResult, ToolError> {
            unimplemented!()
        }
    }

    fn make_tools() -> Arc<Vec<Arc<dyn AgentTool>>> {
        Arc::new(vec![
            Arc::new(MockSearchableTool {
                tool_name: "read".into(),
                tool_description: "Read a file from disk".into(),
            }),
            Arc::new(MockSearchableTool {
                tool_name: "write".into(),
                tool_description: "Write content to a file".into(),
            }),
            Arc::new(MockSearchableTool {
                tool_name: "grep".into(),
                tool_description: "Search file contents with regex".into(),
            }),
        ])
    }

    #[tokio::test]
    async fn test_exact_select() {
        let tool = ToolSearchTool::new(make_tools());
        let result = tool
            .execute(
                "id",
                serde_json::json!({"query": "select:read,grep"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let text = match &result.content[0] {
            Content::Text { text } => text,
            _ => panic!("expected text"),
        };
        assert!(text.contains("read"));
        assert!(text.contains("grep"));
        assert!(!text.contains("write"));
    }

    #[tokio::test]
    async fn test_keyword_search() {
        let tool = ToolSearchTool::new(make_tools());
        let result = tool
            .execute(
                "id",
                serde_json::json!({"query": "file"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let text = match &result.content[0] {
            Content::Text { text } => text,
            _ => panic!("expected text"),
        };
        // "file" appears in read and write descriptions
        assert!(text.contains("read"));
        assert!(text.contains("write"));
    }

    #[tokio::test]
    async fn test_no_results() {
        let tool = ToolSearchTool::new(make_tools());
        let result = tool
            .execute(
                "id",
                serde_json::json!({"query": "nonexistent"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let text = match &result.content[0] {
            Content::Text { text } => text,
            _ => panic!("expected text"),
        };
        assert!(text.contains("No tools found"));
    }
}
