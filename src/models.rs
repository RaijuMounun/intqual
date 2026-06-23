/// The canonical data contract between the Network Engine (Producer) and the UI (Consumer).
/// By strictly decoupling the data model from both business logic and rendering, 
/// we ensure thread-safety and allow future extensions (like writing metrics to a database or REST API)
/// without altering the core engine mechanics.
#[derive(Debug)]
pub struct NetworkMetrics {
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
    pub icmp_ping: Result<f64, String>,
    
    /// The TCP ping latency (handshake duration) in milliseconds.
    /// Follows the same strict error-handling paradigm as `icmp_ping` to detect Application Layer drops.
    pub tcp_ping: Result<f64, String>,
    
    /// UNIX timestamp of when this measurement was dispatched.
    /// While sequence numbers handle UI relative ordering, absolute timestamps are essential 
    /// for historical data logging (e.g., exporting to CSV/JSON) or cross-referencing 
    /// network outages with external system logs.
    pub timestamp: u64,
}