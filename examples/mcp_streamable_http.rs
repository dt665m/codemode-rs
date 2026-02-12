use codemode_rs::prelude::*;
use rmcp::service::ServiceExt;

const TEST_MCP_SERVER: &str = "https://mcpplaygroundonline.com/mcp-echo-server";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let transport = rmcp::transport::StreamableHttpClientTransport::from_uri(TEST_MCP_SERVER);
    let service = ().serve(transport).await?;
    let mcp = McpToolClient::new(service);

    let config = CodeModeClientConfigBuilder::default()
        .sandbox(SandboxConfig::new(tokio::runtime::Handle::current()))
        .build()?;
    let mut client = CodeModeClient::new(config);
    client.register_async_source(mcp.clone(), "test").await?;

    let result = client
        .call_tool_chain(
            "const [echo1, echo2] = await Promise.all([\
                test.echo({\"data\": \"Hello, MCP!\",\"message\": \"Echo1 message\",\"timestamp\": true}),\
                test.echo({\"data\": \"Hello, MCP!\",\"message\": \"Echo2 message\",\"timestamp\": true})
            ]);\
            return { echo1, echo2 };",
        )
        .await?;
    tracing::info!("Result: {}", result.result);

    Ok(())
}
