pub mod models;
pub mod engine;
pub mod network;
pub mod ui;
pub mod probe;
pub mod utils;

use clap::Parser;

use tokio::sync::mpsc;

/// Defines the Command Line Interface (CLI) schema.
/// Using `clap` allows us to define the API declaratively, ensuring POSIX-compliant 
/// argument parsing and auto-generating standard documentation (--help) without manual boilerplate.
#[derive(Parser, Debug)]
#[command(author, version, about = "A zero-runtime-dependency asynchronous network analysis engine", long_about = None)]
struct Cli {
    /// The target IP address or hostname to analyze.
    /// Defaults to Google as a highly available edge node for baseline internet connectivity testing.
    #[arg(default_value = "google.com")]
    target: String,

    /// The target port for TCP handshake measurements.
    /// Defaults to HTTPS (443) because most modern corporate firewalls and ISPs 
    /// allow outbound 443 traffic, drastically reducing the chance of false-positive port blocking.
    #[arg(short, long, default_value_t = 443)]
    port: u16,

    /// The polling interval in milliseconds.
    /// Defines the metronome tick rate for the asynchronous engine.
    #[arg(short, long, default_value_t = 500)]
    interval: u64,

    /// The connection timeout threshold in milliseconds.
    /// Enforces strict cutoffs to prevent zombie tasks from piling up in the Tokio 
    /// reactor if a target network blackholes our packets.
    #[arg(short = 't', long, default_value_t = 1000)]
    timeout: u64,

    /// Use the mock engine (simulates network without real packets)
    #[arg(long)]
    mock: bool,
}

/// The asynchronous application entry point.
/// #[tokio::main] Initializes the async runtime required for multiplexing concurrent 
/// network operations without allocating a heavy OS thread for each individual connection.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let file_appender = tracing_appender::rolling::never(".", "intqual.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|e| {
                    tracing::error!("Fatal startup error (tracing_subscriber): {}", e);
                    std::process::exit(1);
                })
        )
        .with_writer(non_blocking)
        .init();

    // 1. Parse CLI arguments
    let cli = Cli::parse();

    // 2. Establish the telemetry pipeline.
    // MPSC (Multi-Producer, Single-Consumer) Funnels data from the multi-threaded engine to the UI.
    // WHY BOUNDED (100): Implements backpressure. If the UI rendering thread stalls (e.g., OS freeze),
    // the channel won't infinitely expand and cause an OOM (Out Of Memory) crash.
    let (tx, rx) = mpsc::channel(100);
    let (cmd_tx, cmd_rx) = mpsc::channel::<crate::engine::core_engine::EngineCommand>(10);

    // 3. Instantiate the engine with injected configurations.
    // Dependency Injection Keeps the engine pure and testable without hardcoding CLI contexts.
    let engine: Box<dyn engine::NetworkEngine> = if cli.mock {
        Box::new(engine::MockEngine::new())
    } else {
        Box::new(engine::CoreEngine::new(cli.target, cli.port, cli.interval, cli.timeout))
    };

    // 4. Ignite the async engine in the background (Fire and Forget).
    // The engine will spawn its own detached micro-tasks and asynchronously push data into `tx`.
    engine.start(tx, cmd_rx).await;

    // 5. Transfer control of the main OS thread to the UI event loop.
    // WHY: Terminal rendering (crossterm) is inherently synchronous and blocking. 
    // Running it on the main thread ensures stable rendering while Tokio handles I/O in the background.
    ui::run_app(rx, cmd_tx)?;

    Ok(())
}