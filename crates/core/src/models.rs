pub use crate::probe::TelemetryEvent;

#[derive(thiserror::Error, Debug)]
pub enum ProbeError {
    #[error("ICMP Timeout")]
    IcmpTimeout,
    #[error("ICMP Error: Type {type_}, Code {code}")]
    IcmpError { type_: u8, code: u8 },
    #[error("TCP Timeout")]
    TcpTimeout,
    #[error("Socket Error: {0}")]
    Socket(#[from] std::io::Error),
    #[error("Permission Denied / Unsupported")]
    PermissionDenied,
    #[error("DNS Resolution Failed: {0}")]
    DnsResolution(String),
    #[error("Bandwidth Test Failed: {0}")]
    BandwidthTestFailed(String),
    #[error("Rate Limit Exceeded (Ban): {0}")]
    RateLimited(String),
    #[error("Time Synchronization Error")]
    TimeSyncError,
    #[error("Thread Panic: {0}")]
    ThreadPanic(String),
    #[error("Packet Build/Decode Error: {0}")]
    PacketError(String),
    #[error("IPC Communication Error: {0}")]
    IpcError(String),
    /// A `MeasureRequest` outside the accepted bounds. Shared by the headless CLI (exit 2)
    /// and the MCP server, so both reject the same inputs with the same message.
    ///
    /// The message is shown as is: it already says what is wrong, and a "Invalid request:"
    /// prefix stacked on the MCP argument errors ("Invalid request: invalid arguments: ...").
    #[error("{0}")]
    InvalidRequest(String),
    /// A target that is not a hostname or IP address (empty, a URL, `host:port`, shell
    /// characters...). Rejected by `target::parse_target` before any DNS lookup; the message
    /// names the problem and, where it can, the fix.
    #[error("{0}")]
    InvalidTarget(String),
    /// The target has no address of the family asked for with `-4`/`-6` or `ip_version`
    /// ("google.com has no IPv6 address"), or is an IP literal of the other family.
    #[error("{0}")]
    NoAddressForFamily(String),
    /// Something this platform cannot do yet, such as ICMPv6 on Unix. Distinct from
    /// `socket` so a caller does not retry or blame the network.
    #[error("{0}")]
    Unsupported(String),
}

impl ProbeError {
    /// Stable snake_case name of the variant. `ProbeError` wraps `io::Error`, so it cannot be
    /// serialized; machine-readable output stores this name plus the `Display` text instead.
    /// Scripts match on these names, so they must never change once released.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::IcmpTimeout => "icmp_timeout",
            Self::IcmpError { .. } => "icmp_error",
            Self::TcpTimeout => "tcp_timeout",
            Self::Socket(_) => "socket",
            Self::PermissionDenied => "permission_denied",
            Self::DnsResolution(_) => "dns_resolution",
            Self::BandwidthTestFailed(_) => "bandwidth_test_failed",
            Self::RateLimited(_) => "rate_limited",
            Self::TimeSyncError => "time_sync_error",
            Self::ThreadPanic(_) => "thread_panic",
            Self::PacketError(_) => "packet_error",
            Self::IpcError(_) => "ipc_error",
            Self::InvalidRequest(_) => "invalid_request",
            Self::InvalidTarget(_) => "invalid_target",
            Self::NoAddressForFamily(_) => "no_address_for_family",
            Self::Unsupported(_) => "unsupported",
        }
    }
}

/// The canonical data contract between the Network Engine (Producer) and the UI (Consumer).
/// By strictly decoupling the data model from both business logic and rendering,
/// we ensure thread-safety and allow future extensions (like writing metrics to a database or REST API)
/// without altering the core engine mechanics.
#[derive(Debug)]
pub struct PingMetrics {
    /// Incremental ID of the measurement tick.
    /// Network environments are chaotic (especially UDP/ICMP). Packets arrive out-of-order.
    /// The UI relies on this sequence to correctly index data points on the sliding window charts,
    /// ensuring the X-axis (Time) remains perfectly synchronized regardless of delivery order.
    pub sequence_number: u64,

    /// The IP address or hostname being monitored (e.g., "8.8.8.8").
    pub target_ip: String,

    /// The ICMP ping latency in milliseconds.
    /// `Result`: In network diagnostics, a failure (Timeout/Permission Denied) is first-class data.
    /// Using `Result` forces the UI to explicitly handle and display specific OS errors,
    /// preventing "error swallowing" and ensuring high observability.
    /// `None` until the first ICMP sample arrives: ICMP and TCP probes report independently,
    /// and inventing a placeholder error would show a timeout that never happened.
    pub icmp_ping: Option<Result<f64, ProbeError>>,

    /// The TCP ping latency (handshake duration) in milliseconds.
    /// Follows the same strict error-handling paradigm as `icmp_ping` to detect Application Layer drops.
    /// `None` until the first TCP sample arrives, for the same reason as `icmp_ping`.
    pub tcp_ping: Option<Result<f64, ProbeError>>,

    /// UNIX timestamp of when this measurement was dispatched.
    /// While sequence numbers handle UI relative ordering, absolute timestamps are essential
    /// for historical data logging (e.g., exporting to CSV/JSON) or cross-referencing
    /// network outages with external system logs.
    pub timestamp: u64,
}

#[derive(Debug, Clone)]
pub enum BandwidthProgress {
    Downloading {
        current_mbps: f64,
        progress_pct: f64,
    },
    Uploading {
        download_result_mbps: f64,
        current_mbps: f64,
        progress_pct: f64,
    },
    Finished {
        download_mbps: f64,
        upload_mbps: f64,
    },
    Failed(String),
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TracerouteHop {
    pub hop_number: u8,
    pub ip_address: Option<String>,
    pub avg_rtt_ms: Option<f64>,
    pub is_destination: bool,
    pub icmp_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_variants() -> Vec<(ProbeError, &'static str)> {
        vec![
            (ProbeError::IcmpTimeout, "icmp_timeout"),
            (ProbeError::IcmpError { type_: 3, code: 1 }, "icmp_error"),
            (ProbeError::TcpTimeout, "tcp_timeout"),
            (
                ProbeError::Socket(std::io::Error::from(std::io::ErrorKind::ConnectionRefused)),
                "socket",
            ),
            (ProbeError::PermissionDenied, "permission_denied"),
            (ProbeError::DnsResolution("x".into()), "dns_resolution"),
            (
                ProbeError::BandwidthTestFailed("x".into()),
                "bandwidth_test_failed",
            ),
            (ProbeError::RateLimited("x".into()), "rate_limited"),
            (ProbeError::TimeSyncError, "time_sync_error"),
            (ProbeError::ThreadPanic("x".into()), "thread_panic"),
            (ProbeError::PacketError("x".into()), "packet_error"),
            (ProbeError::IpcError("x".into()), "ipc_error"),
            (ProbeError::InvalidRequest("x".into()), "invalid_request"),
            (ProbeError::InvalidTarget("x".into()), "invalid_target"),
            (
                ProbeError::NoAddressForFamily("x".into()),
                "no_address_for_family",
            ),
            (ProbeError::Unsupported("x".into()), "unsupported"),
        ]
    }

    #[test]
    fn probe_error_kind_names_every_variant() {
        let variants = all_variants();
        assert_eq!(variants.len(), 16);
        for (err, expected) in &variants {
            assert_eq!(err.kind(), *expected, "wrong kind for {err:?}");
        }
    }

    #[test]
    fn probe_error_kind_names_are_unique_snake_case() {
        let names: Vec<&str> = all_variants().iter().map(|(e, _)| e.kind()).collect();
        let unique: std::collections::HashSet<&str> = names.iter().copied().collect();
        assert_eq!(unique.len(), 16, "duplicate kind names: {names:?}");
        for name in &names {
            assert!(
                !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{name} is not snake_case"
            );
        }
        assert!(!unique.contains(crate::measure::DROPPED_KIND));
    }
}
