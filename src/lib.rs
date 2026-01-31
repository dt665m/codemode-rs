pub mod client;
pub mod sandbox;
mod schema;
mod tool;
pub mod ts_interface;

#[cfg(feature = "mcp")]
pub mod mcp;

pub mod prelude {
    pub use crate::client::{CodeModeClient, CodeModeClientConfig, CodeModeClientConfigBuilder};
    pub use crate::sandbox::{ExecutionResult, SandboxConfig, SandboxConfigBuilder};
    pub use crate::schema::JsonSchema;
    pub use crate::tool::{
        AsyncToolCaller, SyncToolCaller, Tool, ToolCallError, ToolMetadataProvider,
    };
    pub use crate::ts_interface::ToolInterfaceGenerator;

    #[cfg(feature = "mcp")]
    pub use crate::mcp::{McpToolClient, rmcp};
}
