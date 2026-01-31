use crate::tool::{AsyncToolCaller, Tool, ToolCallError, ToolMetadataProvider};

use async_trait::async_trait;
use dashmap::DashMap;
use std::sync::Arc;
use rmcp::model::{CallToolRequestParams, Content, RawContent, Tool as McpTool};
use rmcp::service::{Peer, RoleClient, RunningService};
use serde_json::{Map, Value};
use thiserror::Error;
use tracing::trace;

pub use rmcp;

#[derive(Debug, Error)]
pub enum McpClientError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("mcp error: {0}")]
    Mcp(String),
    #[error("tool response missing content")]
    EmptyContent,
}

#[derive(Clone)]
pub struct McpToolClient {
    service: Arc<RunningService<RoleClient, ()>>,
    tools: Arc<DashMap<String, Tool>>,
}

impl McpToolClient {
    pub fn new(service: RunningService<RoleClient, ()>) -> Self {
        trace!("mcp client created");
        Self {
            service: Arc::new(service),
            tools: Arc::new(DashMap::new()),
        }
    }

    pub fn peer(&self) -> &Peer<RoleClient> {
        self.service.peer()
    }

    pub async fn refresh_tools(&self) -> Result<Vec<Tool>, McpClientError> {
        let tools = self
            .peer()
            .list_all_tools()
            .await
            .map_err(|err| McpClientError::Mcp(err.to_string()))?;

        let converted = tools
            .into_iter()
            .map(convert_tool)
            .collect::<Vec<Tool>>();
        trace!(count = converted.len(), "mcp client refresh tools");
        self.tools.clear();
        for tool in &converted {
            self.tools.insert(tool.name.clone(), tool.clone());
        }
        Ok(converted)
    }

    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, McpClientError> {
        trace!(tool = name, args = %format_value(&arguments), "mcp call tool");
        let arguments = match arguments {
            Value::Null => None,
            Value::Object(map) => Some(map),
            other => {
                let mut wrapped = Map::new();
                wrapped.insert("value".to_string(), other);
                Some(wrapped)
            }
        };

        let request = CallToolRequestParams {
            meta: None,
            name: name.to_string().into(),
            arguments,
            task: None,
        };

        let result = self
            .peer()
            .call_tool(request)
            .await
            .map_err(|err| McpClientError::Mcp(err.to_string()))?;

        let output = if let Some(structured) = result.structured_content {
            structured
        } else if !result.content.is_empty() {
            contents_to_value(result.content)
        } else {
            return Err(McpClientError::EmptyContent);
        };

        trace!(tool = name, result = %format_value(&output), "mcp call tool result");
        Ok(output)
    }
}

#[async_trait]
impl AsyncToolCaller for McpToolClient {
    async fn call_tool_async(&self, name: &str, args: Value) -> Result<Value, ToolCallError> {
        self.call_tool(name, args)
            .await
            .map_err(|err| ToolCallError::Message(err.to_string()))
    }
}

#[async_trait]
impl ToolMetadataProvider for McpToolClient {
    async fn list_tools(&self) -> Result<Vec<Tool>, ToolCallError> {
        self.refresh_tools()
            .await
            .map_err(|err| ToolCallError::Message(err.to_string()))
    }
}

fn convert_tool(tool: McpTool) -> Tool {
    Tool {
        name: tool.name.to_string(),
        description: tool
            .description
            .map(|value| value.to_string())
            .unwrap_or_default(),
        tags: Vec::new(),
        inputs: Value::Object(tool.input_schema.as_ref().clone()),
        outputs: tool
            .output_schema
            .map(|schema| Value::Object(schema.as_ref().clone()))
            .unwrap_or_else(|| Value::Object(Map::new())),
        is_async: true,
    }
}

fn contents_to_value(contents: Vec<Content>) -> Value {
    if contents.len() == 1 {
        return content_to_value(&contents[0]);
    }

    Value::Array(contents.iter().map(content_to_value).collect())
}

fn content_to_value(content: &Content) -> Value {
    match &content.raw {
        RawContent::Text(text) => {
            if let Ok(parsed) = serde_json::from_str::<Value>(&text.text) {
                parsed
            } else {
                Value::String(text.text.clone())
            }
        }
        RawContent::Image(image) => {
            let mut map = Map::new();
            map.insert("type".to_string(), Value::String("image".to_string()));
            map.insert("data".to_string(), Value::String(image.data.clone()));
            map.insert(
                "mime_type".to_string(),
                Value::String(image.mime_type.clone()),
            );
            Value::Object(map)
        }
        RawContent::Resource(resource) => {
            let mut map = Map::new();
            map.insert("type".to_string(), Value::String("resource".to_string()));
            map.insert(
                "resource".to_string(),
                serde_json::to_value(&resource.resource).unwrap_or(Value::Null),
            );
            Value::Object(map)
        }
        RawContent::Audio(audio) => {
            let mut map = Map::new();
            map.insert("type".to_string(), Value::String("audio".to_string()));
            map.insert("data".to_string(), Value::String(audio.data.clone()));
            map.insert(
                "mime_type".to_string(),
                Value::String(audio.mime_type.clone()),
            );
            Value::Object(map)
        }
        RawContent::ResourceLink(resource) => {
            let mut map = Map::new();
            map.insert(
                "type".to_string(),
                Value::String("resource_link".to_string()),
            );
            map.insert(
                "resource".to_string(),
                serde_json::to_value(resource).unwrap_or(Value::Null),
            );
            Value::Object(map)
        }
    }
}

fn format_value(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unserializable>".to_string())
}
