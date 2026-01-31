use codemode_rs::prelude::*;
use rmcp::service::ServiceExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let transport =
        rmcp::transport::StreamableHttpClientTransport::from_uri("http://localhost:3030/mcp");
    let service = ().serve(transport).await?;
    let mcp = McpToolClient::new(service);

    let config = CodeModeClientConfigBuilder::default()
        .sandbox(SandboxConfig::new(tokio::runtime::Handle::current()))
        .build()?;
    let mut client = CodeModeClient::new(config);
    client.register_async_source(mcp.clone(), "media").await?;

    let result = client
        .call_tool_chain(
            "const [scores, news] = await Promise.all([\
                media.get_live_scores({ sport: 'nfl' }),\
                media.search_news({ query: 'nfl' })\
            ]);\
            return { scores, news };",
        )
        .await?;
    tracing::info!("Result: {}", result.result);

    Ok(())
}
