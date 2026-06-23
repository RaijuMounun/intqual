pub mod models;
pub mod engine;
pub mod network;
pub mod ui;

use clap::Parser;
use engine::CoreEngine;
use tokio::sync::mpsc;

/// Represents the Command Line Interface arguments.
/// The 'clap' crate automatically parses terminal arguments into this struct.
#[derive(Parser, Debug)]
#[command(author, version, about = "A zero-runtime-dependency asynchronous network analysis engine", long_about = None)]
struct Cli {
    /// The target IP address or hostname to analyze
    #[arg(default_value = "google.com")]
    target: String,

    /// The target port for TCP handshake measurements
    #[arg(short, long, default_value_t = 443)]
    port: u16,

    /// The polling interval in milliseconds
    #[arg(short, long, default_value_t = 500)]
    interval: u64,

    /// The connection timeout threshold in milliseconds
    #[arg(short = 't', long, default_value_t = 1000)]
    timeout: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Parse CLI arguments
    let cli = Cli::parse();

    // 2. Establish the MPSC channel (Bounded for backpressure handling)
    let (tx, rx) = mpsc::channel(100);

    // 3. Instantiate the engine with injected configurations
    let core_engine = CoreEngine::new(cli.target, cli.port, cli.interval, cli.timeout);

    // 4. Ignite the async engine in the background (Fire and Forget)
    core_engine.start(tx).await;

    // 5. Transfer the main thread to the UI event loop.
    // This function is blocking and consumes metrics from the receiver.
    ui::run_app(rx)?;

    Ok(())
}