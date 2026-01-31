use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::schema::JsonSchema;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub inputs: JsonSchema,
    pub outputs: JsonSchema,
    #[serde(default)]
    pub is_async: bool,
}

#[derive(Debug, Error)]
pub enum ToolCallError {
    #[error("tool call failed: {0}")]
    Message(String),
}

#[async_trait]
pub trait AsyncToolCaller: Send + Sync {
    async fn call_tool_async(&self, name: &str, args: Value) -> Result<Value, ToolCallError>;
}

#[async_trait]
pub trait ToolMetadataProvider: Send + Sync {
    async fn list_tools(&self) -> Result<Vec<Tool>, ToolCallError>;
}

pub trait SyncToolCaller: Send + Sync {
    fn call_tool_sync(&self, name: &str, args: Value) -> Result<Value, ToolCallError>;
}
