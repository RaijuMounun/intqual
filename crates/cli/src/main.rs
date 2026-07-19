pub mod ui;
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

    /// Internal worker mode execution flag
    #[arg(long, hide = true)]
    worker_mode: Option<String>,
}

/// The asynchronous application entry point.
/// #[tokio::main] Initializes the async runtime required for multiplexing concurrent 
/// network operations without allocating a heavy OS thread for each individual connection.
#[tokio::main]
async fn main() -> Result<(), ()> {
    let file_appender = tracing_appender::rolling::never(".", "intqual.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
        )
        .with_writer(non_blocking)
        .init();

    // 1. Parse CLI arguments
    let cli = Cli::parse();

    if let Some(mode) = &cli.worker_mode
        && mode == "traceroute"
    {
        let addr_string = format!("{}:{}", cli.target, cli.port);
        let resolved_addr = match tokio::net::lookup_host(&addr_string).await {
            Ok(mut addrs) => addrs.next().unwrap_or_else(|| {
                tracing::error!("Worker DNS error: no addresses found");
                std::process::exit(1);
            }),
            Err(e) => {
                tracing::error!("Worker DNS error: {}", e);
                std::process::exit(1);
            }
        };
        
        let icmp_identifier = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as u16;

        let (tx, mut rx) = mpsc::channel(100);
        let token = tokio_util::sync::CancellationToken::new();
        let mut probe = intqual_core::probe::traceroute::TracerouteProbe::new(
            resolved_addr,
            std::time::Duration::from_millis(1000),
            30,
            icmp_identifier,
        );

        tokio::spawn(async move {
            use intqual_core::probe::NetworkProbe;
            if let Err(e) = probe.run(tx, token).await {
                tracing::error!("Worker traceroute error: {}", e);
            }
        });

        let mut hops = Vec::new();
        while let Some(event) = rx.recv().await {
            match event {
                intqual_core::models::TelemetryEvent::TracerouteHop(hop) => {
                    hops.push(hop);
                }
                intqual_core::models::TelemetryEvent::TracerouteComplete => {
                    break;
                }
                intqual_core::models::TelemetryEvent::Fatal(e) => {
                    tracing::error!("Fatal error in worker: {}", e);
                    std::process::exit(1);
                }
                _ => {}
            }
        }

        if let Ok(json) = serde_json::to_string(&hops) {
            println!("{}", json);
        }
        std::process::exit(0);
    }

    // 2. Establish the telemetry pipeline.
    let (tx, rx) = mpsc::channel(100);
    let (cmd_tx, cmd_rx) = mpsc::channel::<intqual_core::engine::core_engine::EngineCommand>(10);

    // 3 & 4. Instantiate and ignite the async engine without dynamic dispatch.
    use intqual_core::engine::NetworkEngine;
    if cli.mock {
        let engine = intqual_core::engine::MockEngine;
        engine.start(tx.clone(), cmd_rx).await;
    } else {
        let engine = intqual_core::engine::CoreEngine::new(cli.target, cli.port, cli.interval, cli.timeout);
        engine.start(tx.clone(), cmd_rx).await;
    }

    // 5. Transfer control of the main OS thread to the UI event loop.
    if let Err(e) = ui::run_app(rx, cmd_tx, tx) {
        tracing::error!("Fatal UI error: {}", e);
        std::process::exit(1);
    }

    Ok(())
}