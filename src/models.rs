#[derive(thiserror::Error, Debug)]
pub enum ProbeError {
    #[error("ICMP Timeout")]
    IcmpTimeout,
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
    pub icmp_ping: Result<f64, ProbeError>,
    
    /// The TCP ping latency (handshake duration) in milliseconds.
    /// Follows the same strict error-handling paradigm as `icmp_ping` to detect Application Layer drops.
    pub tcp_ping: Result<f64, ProbeError>,
    
    /// UNIX timestamp of when this measurement was dispatched.
    /// While sequence numbers handle UI relative ordering, absolute timestamps are essential 
    /// for historical data logging (e.g., exporting to CSV/JSON) or cross-referencing 
    /// network outages with external system logs.
    pub timestamp: u64,
}

#[derive(Debug, Clone)]
pub enum BandwidthProgress {
    Downloading { current_mbps: f64, progress_pct: f64 },
    Uploading { download_result_mbps: f64, current_mbps: f64, progress_pct: f64 },
    Finished { download_mbps: f64, upload_mbps: f64 },
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct TracerouteHop {
    pub hop_number: u8,
    pub ip_address: Option<String>,
    pub avg_rtt_ms: Option<f64>,
    pub is_destination: bool,
}
