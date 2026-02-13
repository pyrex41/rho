use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::types::ToolResult;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolError {
    InvalidParameters(String),
    ExecutionFailed(String),
    Timeout,
    Cancelled,
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolError::InvalidParameters(msg) => write!(f, "Invalid parameters: {}", msg),
            ToolError::ExecutionFailed(msg) => write!(f, "Execution failed: {}", msg),
            ToolError::Timeout => write!(f, "Tool execution timed out"),
            ToolError::Cancelled => write!(f, "Tool execution was cancelled"),
        }
    }
}

impl std::error::Error for ToolError {}

#[async_trait]
pub trait AgentTool: Send + Sync {
    fn name(&self) -> &str;
    fn label(&self) -> String;
    fn description(&self) -> String;
    fn parameters_schema(&self) -> Value;

    async fn execute(&self, tool_call_id: &str, params: Value, cancel: CancellationToken) -> Result<ToolResult, ToolError>;
}