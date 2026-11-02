//! Run the stage-7 MCP server over newline-delimited stdio JSON-RPC.
use rustscrapling_mcp::{serve_stdio, RustScraplingMcp};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    serve_stdio(RustScraplingMcp::default()).await
}
