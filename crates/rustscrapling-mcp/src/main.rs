//! Run the stage-7 MCP server over newline-delimited stdio JSON-RPC.
use rmcp::ServiceExt;
use rustscrapling_mcp::RustScraplingMcp;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = RustScraplingMcp.serve(rmcp::transport::stdio()).await?;
    server.waiting().await?;
    Ok(())
}
