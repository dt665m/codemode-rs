use std::collections::HashMap;
use std::sync::Arc;

use derive_builder::Builder;
use serde_json::Value;
use tracing::{debug, trace};

use crate::sandbox::{ExecutionResult, Sandbox, SandboxConfig, SandboxError};
use crate::tool::{AsyncToolCaller, SyncToolCaller, Tool, ToolCallError, ToolMetadataProvider};
use crate::ts_interface::ToolInterfaceGenerator;

#[derive(Clone, Builder)]
#[builder(pattern = "owned")]
pub struct CodeModeClientConfig {
    #[builder(setter(custom))]
    #[builder(default)]
    pub callers: HashMap<String, ToolCallerEntry>,
    #[builder(setter(custom))]
    pub sandbox: SandboxConfig,
}

impl CodeModeClientConfigBuilder {
    pub fn sandbox(mut self, sandbox: SandboxConfig) -> Self {
        self.sandbox = Some(sandbox);
        self
    }
}

pub struct CodeModeClient {
    callers: HashMap<String, ToolCallerEntry>,
    sandbox: Sandbox,
    interface_generator: ToolInterfaceGenerator,
}

impl CodeModeClient {
    pub fn new(config: CodeModeClientConfig) -> Self {
        trace!("codemode client initialized");
        Self {
            callers: config.callers,
            sandbox: Sandbox::new(config.sandbox),
            interface_generator: ToolInterfaceGenerator::default(),
        }
    }

    pub fn get_tool(&self, name: &str) -> Option<&Tool> {
        trace!(tool = name, "codemode get_tool");
        self.callers.get(name).map(|entry| &entry.tool)
    }

    pub fn get_tools(&self) -> Vec<&Tool> {
        let tools: Vec<&Tool> = self.callers.values().map(|entry| &entry.tool).collect();
        trace!(count = tools.len(), "codemode get_tools");
        tools
    }

    pub fn register_async_tool(
        &mut self,
        mut tool: Tool,
        raw_name: String,
        caller: Arc<dyn AsyncToolCaller>,
    ) {
        tool.is_async = true;
        let name = tool.name.clone();
        let entry = ToolCallerEntry {
            tool,
            raw_name,
            caller: CallerKind::Async(caller),
        };
        if self.callers.insert(name.clone(), entry).is_some() {
            trace!(tool = name.as_str(), "tool caller overwritten");
        }
    }

    pub async fn register_async_source<S>(
        &mut self,
        source: S,
        prefix: &str,
    ) -> Result<(), ToolCallError>
    where
        S: AsyncToolCaller + ToolMetadataProvider + Clone + 'static,
    {
        let tools = source.list_tools().await?;
        let caller = Arc::new(source);
        for mut tool in tools {
            let raw_name = tool.name.clone();
            tool.name = apply_prefix(prefix, &tool.name);
            self.register_async_tool(tool, raw_name, caller.clone());
        }
        Ok(())
    }

    pub fn register_sync_tool(
        &mut self,
        mut tool: Tool,
        raw_name: String,
        caller: Arc<dyn SyncToolCaller>,
    ) {
        tool.is_async = false;
        let name = tool.name.clone();
        let entry = ToolCallerEntry {
            tool,
            raw_name,
            caller: CallerKind::Sync(caller),
        };
        if self.callers.insert(name.clone(), entry).is_some() {
            trace!(tool = name.as_str(), "tool caller overwritten");
        }
    }

    pub async fn register_sync_source<S>(
        &mut self,
        source: S,
        prefix: &str,
    ) -> Result<(), ToolCallError>
    where
        S: SyncToolCaller + ToolMetadataProvider + Clone + 'static,
    {
        let tools = source.list_tools().await?;
        let caller = Arc::new(source);
        for mut tool in tools {
            let raw_name = tool.name.clone();
            tool.name = apply_prefix(prefix, &tool.name);
            self.register_sync_tool(tool, raw_name, caller.clone());
        }
        Ok(())
    }

    pub fn tool_to_typescript_interface(&self, tool: &Tool) -> String {
        trace!(
            tool = tool.name.as_str(),
            "codemode tool_to_typescript_interface"
        );
        self.interface_generator.tool_to_typescript_interface(tool)
    }

    pub fn get_all_tools_typescript_interfaces(&self) -> String {
        let tools = self.get_tools();
        trace!(
            count = tools.len(),
            "codemode get_all_tools_typescript_interfaces"
        );
        let interfaces = tools
            .iter()
            .map(|tool| self.interface_generator.tool_to_typescript_interface(tool))
            .collect::<Vec<String>>();
        format!(
            "// Auto-generated TypeScript interfaces for UTCP tools\n{}",
            interfaces.join("\n\n")
        )
    }

    pub async fn call_tool_chain(&self, code: &str) -> Result<ExecutionResult, SandboxError> {
        let tools = self.get_tools();
        debug!(
            code = code,
            tool_count = tools.len(),
            "codemode call_tool_chain"
        );
        let sandbox = &self.sandbox;
        let interface_generator = &self.interface_generator;
        let code = code.to_string();
        let result = tokio::task::block_in_place(|| {
            sandbox.execute(&code, &tools, interface_generator, &self.callers)
        })?;
        debug!(
            result = %format_value(&result.result),
            "codemode call_tool_chain result"
        );
        Ok(result)
    }
}

#[derive(Clone)]
pub struct ToolCallerEntry {
    pub tool: Tool,
    pub raw_name: String,
    pub caller: CallerKind,
}

#[derive(Clone)]
pub enum CallerKind {
    Async(Arc<dyn AsyncToolCaller>),
    Sync(Arc<dyn SyncToolCaller>),
}

fn format_value(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unserializable>".to_string())
}

fn apply_prefix(prefix: &str, name: &str) -> String {
    format!("{}.{}", prefix, name)
}
