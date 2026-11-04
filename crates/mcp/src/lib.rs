//! MCP (Model Context Protocol) server for intqual: lets an AI assistant run `ping`,
//! `tcp_latency` and `traceroute` measurements and get the same JSON report as
//! `intqual --count N --json`. Spec: `docs/specs/mcp-server.md`.
//!
//! stdio only: stdout is the JSON-RPC stream, so nothing in this crate prints. Logs go through
//! `tracing`; the binary routes them to stderr.

pub mod args;
pub mod server;

pub use intqual_core::measure::Source;
pub use server::{CoreMeasurer, IntqualServer, Measurer};

use rmcp::ServiceExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerOptions {
    /// `Source::Mock` serves simulated data with no network traffic (`intqual mcp --mock`).
    pub source: Source,
}

/// Why the server stopped with an error. A host closing stdin after the handshake is not an
/// error.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// The `initialize` handshake failed (for example stdin closed before it).
    #[error("MCP server failed to start: {0}")]
    Start(String),
    /// The service task ended abnormally.
    #[error("MCP server stopped with an error: {0}")]
    Run(String),
}

/// Serves `IntqualServer` on stdin/stdout until the host closes the connection.
pub async fn serve_stdio(options: ServerOptions) -> Result<(), ServeError> {
    let running = match IntqualServer::new(options.source)
        .serve(rmcp::transport::stdio())
        .await
    {
        Ok(running) => running,
        Err(e) => {
            tracing::error!("MCP server failed to start: {}", e);
            return Err(ServeError::Start(e.to_string()));
        }
    };
    match running.waiting().await {
        Ok(reason) => {
            tracing::info!("MCP server stopped: {:?}", reason);
            Ok(())
        }
        Err(e) => {
            tracing::error!("MCP server task failed: {}", e);
            Err(ServeError::Run(e.to_string()))
        }
    }
}
