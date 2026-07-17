pub mod ping;
pub mod tcp;
pub mod traceroute;

use crate::models::{ProbeError, BandwidthProgress, TracerouteHop};

#[derive(Debug)]
pub enum TelemetryEvent {
    Ping { sequence_number: u64, target_ip: String, result: Result<f64, ProbeError>, timestamp: u64 },
    Tcp { sequence_number: u64, target_ip: String, result: Result<f64, ProbeError>, timestamp: u64 },
    Bandwidth(BandwidthProgress),
    BandwidthError(ProbeError),
    TracerouteHop(TracerouteHop),
    TracerouteComplete,
    TracerouteError(ProbeError),
    DnsResolved { ip: String, hostname: Option<String> },
    Fatal(ProbeError),
}

pub trait NetworkProbe: Send + Sync {
    fn run(&mut self, tx: tokio::sync::mpsc::Sender<TelemetryEvent>, cancel_token: tokio_util::sync::CancellationToken) -> impl std::future::Future<Output = Result<(), ProbeError>> + Send;
}
